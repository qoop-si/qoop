//! What a supervising agent needs beyond reading and prompting: hearing that a
//! terminal changed, starting and ending terminals, pressing named keys, and
//! refusing to write to a process it did not read.
//!
//! Every verb here is a client of something the daemon already serves: the
//! event lane, `ListAgents`, `CreateTerminal`, `CloseTerminal` and the
//! terminal input lane. None of them adds a control method.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use qoro_runtime::client;
use qoro_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, WorkspaceRow,
};
use qoro_runtime::daemon::ClientWire;
use qoro_runtime::paths;
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
const TIMED_OUT: i32 = 124;

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
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        // A missing or dead socket is a session nobody started, said as such: these
        // verbs never start a daemon, and "No such file or directory" names no fix.
        let (stream, inbuf) = client::connect(&socket, who)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    std::io::Error::other("no session running: `qoro` starts one")
                }
                _ => e,
            })?;
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

    /// A space by id or name, made when the name is new.
    ///
    /// The list and the create hold a lock in the session's directory, so two callers
    /// naming the same new space make it once: the daemon makes a space per request,
    /// which the interface relies on, and eight parallel `new --space x` made three
    /// spaces called `x` without it.
    async fn space(&mut self, wanted: &str) -> std::io::Result<i64> {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let _held = qoro_runtime::lock::StartupLock::acquire(&dir.join("spaces.lock"))?;
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
            (client::Incoming::Render { terminal, snapshot }, Some((watched, text)))
                if terminal == watched =>
            {
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
                return Err(std::io::Error::other("the daemon closed the connection"));
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
/// `qoro prompt 3 explain --generation 5 to me` is a sentence about the flag,
/// not the flag. Anything that is not a number after it is left as text.
pub(crate) fn take_generation(args: &mut Vec<String>) -> Option<u64> {
    let [.., flag, value] = args.as_slice() else {
        return None;
    };
    if flag != "--generation" {
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
        let (rows, _) = Link::open("qoro-generation").await?.agents().await?;
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
fn own_terminal(home: Option<&str>, session: &str) -> Option<u32> {
    let terminal = std::env::var("QORO_TERMINAL").ok()?.parse().ok()?;
    (home == Some(session)).then_some(terminal)
}

/// `qoro wait`: block until a terminal reaches a state.
///
/// Level triggered: a
/// target already there returns at once. Exits 124 on timeout, 1 when a
/// target closes or nothing can be waited on.
pub(crate) fn run_wait(args: &[String], home: Option<&str>, session: &str) -> std::io::Result<()> {
    let help = "usage: qoro wait [target...] [--until STATE[,STATE...]] [--match TEXT] [--timeout SECS] [--json]";
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
    let own = own_terminal(home, session);

    let rt = tokio::runtime::Runtime::new()?;
    let matched = rt.block_on(async {
        let mut link = Link::open("qoro-wait").await?;
        let (rows, _) = link.agents().await?;
        let watched: Vec<u32> = if targets.is_empty() {
            rows.iter()
                .filter(|r| r.agent && Some(r.terminal) != own)
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
            "qoro: timed out after {}s waiting for {what}",
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
    let mut link = Link::open("qoro-prompt").await?;
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

/// Print how a `prompt --wait` ended and exit with the status `qoro wait` uses.
pub(crate) fn finish_turn(waited: TurnWait) -> std::io::Result<()> {
    match waited {
        TurnWait::Done(row) => {
            println!("{}", super::agent_line(&row));
            Ok(())
        }
        TurnWait::TimedOut(row) => {
            match row {
                Some(row) => eprintln!("qoro: timed out; last seen {}", super::agent_line(&row)),
                None => eprintln!("qoro: timed out"),
            }
            std::process::exit(TIMED_OUT);
        }
    }
}

/// Parsed `qoro new` arguments, shared with `qoro worktree new`.
#[derive(Default)]
pub(crate) struct NewOpts {
    pub(crate) name: Option<String>,
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// A space id, or a name, which is made when nobody has used it yet.
    pub(crate) space: Option<String>,
    pub(crate) command: Vec<String>,
}

const NEW_USAGE: &str =
    "usage: qoro new [--name NAME] [--cwd DIR] [--space ID|NAME] [-- command...]";

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

/// `qoro new`: open a terminal, optionally typing a command into its shell.
///
/// Prints only the terminal number, so `t=$(qoro new ...)` works. The
/// command is typed rather than spawned, the same path a person takes: the
/// daemon starts the user's shell and nothing else (ADR-015), and the shell
/// gives the command its environment.
pub(crate) fn run_new(args: &[String]) -> std::io::Result<()> {
    let opts = parse_new_args(args).map_err(super::usage)?;
    open_terminal(&opts, |terminal| println!("{terminal}"))
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
        let mut link = Link::open("qoro-new").await?;
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
        if opts.command.is_empty() {
            return Ok(());
        }
        // Typed once the shell has drawn something and gone quiet: a process
        // the daemon can name is not yet a line editor reading its input, and
        // a slow rc file runs in between. `fetch_snapshot` settles for half a
        // second, which is the quiet part.
        let deadline = Instant::now() + SHELL_PATIENCE;
        loop {
            let screen = super::fetch_snapshot(terminal, "qoro-new").await?;
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
        let line: Vec<String> = opts.command.iter().map(|w| shell_quote(w)).collect();
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

/// `qoro close`: end a terminal and what it started, as `ctrl-a x` does.
///
/// A number and never a name: a destructive verb does not guess. The
/// terminal this runs in is refused, because a supervisor that closes
/// itself has lost the loop it was running.
pub(crate) fn run_close(args: &[String], home: Option<&str>, session: &str) -> std::io::Result<()> {
    let help = "usage: qoro close <terminal> [--generation G]";
    let mut args = args.to_vec();
    let generation = take_generation(&mut args);
    let terminal: u32 = match args.get(2..) {
        Some([n]) => n.parse().map_err(|_| super::usage(help))?,
        _ => return Err(super::usage(help)),
    };
    if own_terminal(home, session) == Some(terminal) {
        return Err(std::io::Error::other(
            "refusing to close the terminal this command runs in",
        ));
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut link = Link::open("qoro-close").await?;
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
                println!("closed terminal {terminal}");
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

/// `qoro send <terminal> --key NAME...`: press named keys, one frame each.
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
            super::fetch_snapshot(terminal, "qoro-send")
                .await?
                .modes
                .application_cursor
        } else {
            false
        };
        let mut link = Link::open("qoro-send").await?;
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
    fn frames(channel: qoro_proto::Channel, stream: u32, body: &str) -> Vec<u8> {
        let mut out = bytes::BytesMut::new();
        for frame in qoro_proto::chunk(
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
            qoro_proto::Channel::Terminal,
            3,
            r#"{"cols":80,"rows":24,"cursor":[0,0],"lines":["$ echo STATIC_MARK_7","STATIC_MARK_7"]}"#,
        );
        wire.extend(frames(
            qoro_proto::Channel::Control,
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
            qoro_proto::Channel::Control,
            0,
            r#"{"op":"Subscribed","id":"sub-1","terminals":[3]}"#,
        );
        wire.extend(frames(
            qoro_proto::Channel::Terminal,
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
        let mut args = words("qoro prompt 3 hello --generation 7");
        assert_eq!(take_generation(&mut args), Some(7));
        assert_eq!(args, words("qoro prompt 3 hello"));
        for text in [
            "qoro prompt 3 explain --generation 5 to me",
            "qoro prompt 3 set --generation high",
            "qoro prompt 3 --generation",
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
            cwd: None,
            slept_at_unix: None,
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
                id: 1,
                name: "default".into(),
                terminals: 1,
            },
            WorkspaceRow {
                id: 2,
                name: "work".into(),
                terminals: 0,
            },
        ];
        assert_eq!(pick_space(&spaces, "2").unwrap(), Some(2));
        assert_eq!(pick_space(&spaces, "work").unwrap(), Some(2));
        assert_eq!(pick_space(&spaces, "fresh").unwrap(), None);
        assert!(pick_space(&spaces, "7").is_err());
        let named = vec![WorkspaceRow {
            id: 3,
            name: "2024".into(),
            terminals: 0,
        }];
        assert_eq!(
            pick_space(&named, "2024").unwrap(),
            Some(3),
            "a number that is no id is tried as a name"
        );
    }

    #[test]
    fn new_takes_a_space_by_name_and_everything_after_the_separator() {
        let argv = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let opts = parse_new_args(&argv(&[
            "qoro", "new", "--space", "work", "--name", "fix", "--", "claude", "--resume",
        ]))
        .unwrap();
        assert_eq!(opts.space.as_deref(), Some("work"));
        assert_eq!(opts.name.as_deref(), Some("fix"));
        assert_eq!(opts.command, vec!["claude", "--resume"]);
        let missing = parse_new_args(&argv(&["qoro", "new", "--cwd"]));
        assert!(missing.is_err_and(|e| e.starts_with("missing value for --cwd")));
    }
}
