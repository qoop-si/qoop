//! The chat of an agent terminal: `Messages`, `MessageDetail` and `MessagesChanged`.
//!
//! The daemon owns the disk the agent writes its session to, so it parses (`qoop_parsed`)
//! and a client, local or remote, only draws. A read is a tail parse of a few to a few tens of
//! milliseconds, on the chat pool (two threads, see `blocking::chat`) and never on the read
//! loop. A flood costs two threads and a refusal each: no permit free answers `unavailable`
//! at once, a terminal has at most [`MAX_QUEUED`] reads waiting on its turn, and the last
//! page of a watched terminal is kept while its file is unchanged.
//!
//! No file watcher exists, so a terminal a client asked about is stat-ed every 200 ms for 30
//! seconds after its last question, and a changed stamp is said as an event: the first change
//! at once, so a message sent shows without waiting, then at most once a second while the file
//! keeps growing, so every reader reads no more often than that. With nobody asking the task
//! is parked and the daemon wakes for nothing.
//!
//! The stamp of an OpenCode session is the stamp of the shared database (and its `-wal`), so
//! any OpenCode write says the chat of every watched OpenCode terminal changed. A client that
//! reads again finds the same page; that is the price of not parsing to find out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qoop_parsed::{Detail, Digest, MAX_SKIP, Options, Source};

use crate::agents::AgentRegistry;
use crate::control::{ControlRequest, ControlResponse, EventMessage};
use crate::daemon::{TerminalRegistry, agent_dirs, agent_program};

/// How long a terminal stays watched after the last `Messages` or `MessageDetail` for it.
const WATCH_FOR: Duration = Duration::from_secs(30);

/// How often a watched terminal's session file is looked at. A stat each, on the chat pool.
const TICK: Duration = Duration::from_millis(200);

/// The least time between two `MessagesChanged` for one terminal: each sends its readers to
/// parse the page again.
const NEWS_EVERY: Duration = Duration::from_secs(1);

/// How often a watched terminal whose file is not there yet is looked for.
const RELOCATE: Duration = Duration::from_secs(5);

/// What a disk is given to answer a client's read, and the watch's look.
const DEADLINE: Duration = Duration::from_secs(5);
const TICK_DEADLINE: Duration = Duration::from_secs(2);

/// The most items one page may ask for; the page's own byte budget cuts it shorter.
pub const MAX_LIMIT: u32 = 200;

/// The default and the largest cut of one side of a tool call.
const DETAIL_DEFAULT: u32 = 64 * 1024;
const DETAIL_MAX: u32 = 1024 * 1024;

/// The most reads of one terminal in the daemon at once, the one running included. More are
/// refused at once: a client that asks faster than a parse is not waited for.
const MAX_QUEUED: usize = 4;

/// When the agent now in `terminal` started, in unix ms: its process's own start time where the
/// system gives one (macOS), else when the daemon first saw that leader in the foreground, kept
/// across a stop and resume. Work the session file shows as running from before that was left
/// behind by an agent that is gone. The same value every read, so it can key a memo.
fn alive_since(agents: &AgentRegistry, terminal: u32) -> Option<i64> {
    let agent = agents.agent(terminal)?;
    #[cfg(target_os = "macos")]
    if let Some(started) = agent.leader.and_then(qoop_pty::sys::start_time_of) {
        return i64::try_from(started).ok().map(|s| s.saturating_mul(1000));
    }
    agent.leader_seen_ms()
}

/// What one read answers with, and what the memo keeps of it.
enum Reply {
    Page(Digest),
    Detail(Detail),
}

/// What a reply was for. A read with the same key, while the file's stamp is still
/// `revision`, is the same reply.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    revision: u64,
    skip: usize,
    limit: usize,
    max_bytes: usize,
    tool_id: Option<String>,
    /// When the agent that serves the page started: a new one reads running work as left behind.
    alive_since: Option<i64>,
    /// The subagent whose chat it is, `None` for the terminal's own.
    child: Option<String>,
}

#[derive(Clone)]
struct Memo {
    key: Key,
    reply: Arc<Reply>,
}

/// What the daemon knows of one watched terminal.
struct Watched {
    asked: Instant,
    /// The conversation `source` was found for.
    session: String,
    source: Option<Source>,
    /// The stamp last seen, or the one current when the watch began. `None` while no file
    /// has been seen.
    revision: Option<u64>,
    /// Not before this is a missing file looked for again.
    next_locate: Option<Instant>,
    /// The last reply, for a read that asks the same while the stamp has not moved.
    memo: Option<Memo>,
    /// The same for the last read of one of its subagents' chats (its key names the child).
    child_memo: Option<Memo>,
    /// When the agent the last page was made for started: the stamp of that page folds it in
    /// (`qoop_parsed::revision_with`), so the watch must too.
    alive: Option<i64>,
    /// When a change was last said: the next waits until [`NEWS_EVERY`] after it.
    told: Option<Instant>,
}

/// The terminals a client asked about lately, and the news about their chats.
pub struct Watch {
    watched: Mutex<HashMap<u32, Watched>>,
    /// Rung when a terminal is added, so the task parks while there is none.
    wake: tokio::sync::Notify,
    /// `(terminal, revision)`, fanned out to every connection's event task. Lossy like
    /// `focus`: a client that missed one is told to read again.
    changed: tokio::sync::broadcast::Sender<(u32, u64)>,
    /// Whether the last look of the watch is still running: a hung disk is not asked again
    /// every tick.
    looking: Arc<AtomicBool>,
    /// Reads of a terminal in the daemon: the turn lock and how many hold or wait for it.
    flights: Mutex<HashMap<u32, Flight>>,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            watched: Mutex::default(),
            wake: tokio::sync::Notify::new(),
            changed: tokio::sync::broadcast::channel(16).0,
            looking: Arc::default(),
            flights: Mutex::default(),
        }
    }
}

/// The turn lock of a terminal and how many reads hold or wait for it.
type Flight = (Arc<tokio::sync::Mutex<()>>, usize);

/// A read of one terminal that is in the daemon, by its place in line.
struct Ticket<'a> {
    watch: &'a Watch,
    terminal: u32,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl Ticket<'_> {
    /// Wait for the reads of this terminal ahead of this one. The first of two identical
    /// reads leaves its reply in the memo, so the second one costs a stat.
    async fn turn(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.turn.clone().lock_owned().await
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        let mut flights = self.watch.flights.lock().unwrap();
        if let Some((_, users)) = flights.get_mut(&self.terminal) {
            *users -= 1;
            if *users == 0 {
                flights.remove(&self.terminal);
            }
        }
    }
}

/// What the watch looks at for one terminal.
struct Job {
    terminal: u32,
    program: String,
    session: String,
    source: Option<Source>,
    alive: Option<i64>,
}

/// What the look found.
struct Seen {
    terminal: u32,
    session: String,
    source: Option<Source>,
    revision: Option<u64>,
}

impl Watch {
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(u32, u64)> {
        self.changed.subscribe()
    }

    fn enter(&self, terminal: u32) -> Option<Ticket<'_>> {
        let mut flights = self.flights.lock().unwrap();
        let (turn, users) = flights
            .entry(terminal)
            .or_insert_with(|| (Arc::default(), 0));
        if *users >= MAX_QUEUED {
            return None;
        }
        *users += 1;
        Some(Ticket {
            watch: self,
            terminal,
            turn: turn.clone(),
        })
    }

    /// What is known of a terminal's chat, to save a read the work: where its file is, and the
    /// last reply of the terminal's own chat, or of a subagent's for a `child` read.
    fn known(&self, terminal: u32, session: &str, child: bool) -> (Option<Source>, Option<Memo>) {
        match self.watched.lock().unwrap().get(&terminal) {
            Some(w) if w.session == session => {
                let memo = if child { &w.child_memo } else { &w.memo };
                (w.source.clone(), memo.clone())
            }
            _ => (None, None),
        }
    }

    /// A client read a subagent's chat of `terminal` just now: the watch of the terminal's own
    /// chat goes on (its stamp folds in the newest subagents), and the reply is kept. Where the
    /// terminal's file is, its stamp and its own memo are the terminal's chat's, never changed here.
    fn renew(&self, terminal: u32, session: &str, memo: Option<Memo>) {
        if let Some(w) = self.watched.lock().unwrap().get_mut(&terminal)
            && w.session == session
        {
            w.asked = Instant::now();
            if memo.is_some() {
                w.child_memo = memo;
            }
        }
    }

    /// A client asked about `terminal` just now. `revision` is the stamp of the file as the
    /// read saw it (none when there was no file), kept only when the terminal was not
    /// watched yet: a later stamp is the watch's to find, so that every client that asked
    /// before it hears of it.
    fn mark(
        &self,
        terminal: u32,
        session: &str,
        source: Option<Source>,
        revision: Option<u64>,
        memo: Option<Memo>,
        alive: Option<i64>,
    ) {
        let mut watched = self.watched.lock().unwrap();
        let entry = watched.entry(terminal).or_insert_with(|| Watched {
            asked: Instant::now(),
            session: session.to_string(),
            source: None,
            revision,
            next_locate: None,
            memo: None,
            child_memo: None,
            alive: None,
            told: None,
        });
        entry.asked = Instant::now();
        if entry.session != session {
            *entry = Watched {
                asked: entry.asked,
                session: session.to_string(),
                source: None,
                revision,
                next_locate: None,
                memo: None,
                child_memo: None,
                alive: None,
                told: None,
            };
        }
        entry.next_locate = source.is_none().then(|| Instant::now() + RELOCATE);
        entry.source = source;
        if memo.is_some() {
            entry.memo = memo;
        }
        if alive.is_some() {
            entry.alive = alive;
        }
        drop(watched);
        self.wake.notify_one();
    }

    /// The terminals to look at at `now`, after the ones that are done are dropped: not asked
    /// about for 30 seconds, ended, no longer an agent, or without a conversation. `who` says
    /// the program and the conversation of an agent terminal.
    fn due_with(&self, now: Instant, who: impl Fn(u32) -> Option<(String, String)>) -> Vec<Job> {
        let mut watched = self.watched.lock().unwrap();
        watched.retain(|terminal, w| {
            now.duration_since(w.asked) < WATCH_FOR && who(*terminal).is_some()
        });
        watched
            .iter_mut()
            .filter_map(|(terminal, w)| {
                let (program, session) = who(*terminal)?;
                // A conversation that moved on leaves the old file and its stamp behind.
                if w.session != session {
                    *w = Watched {
                        asked: w.asked,
                        session: session.clone(),
                        source: None,
                        revision: None,
                        next_locate: None,
                        memo: None,
                        child_memo: None,
                        alive: None,
                        told: None,
                    };
                }
                if w.source.is_none() && w.next_locate.is_some_and(|at| now < at) {
                    return None;
                }
                Some(Job {
                    terminal: *terminal,
                    program,
                    session,
                    source: w.source.clone(),
                    alive: w.alive,
                })
            })
            .collect()
    }

    /// Look at every watched terminal once, and say the chat of each whose file changed.
    pub async fn tick(&self, terminals: &TerminalRegistry, agents: &Arc<AgentRegistry>) {
        self.tick_with(Instant::now(), |terminal| {
            who(terminals, agents, terminal).ok()
        })
        .await;
    }

    async fn tick_with(&self, now: Instant, who: impl Fn(u32) -> Option<(String, String)>) {
        // The look before is still blocked on a disk: it is not asked again, nor does it get
        // company on the chat pool.
        if self.looking.swap(true, Ordering::AcqRel) {
            return;
        }
        let jobs = self.due_with(now, who);
        if jobs.is_empty() {
            self.looking.store(false, Ordering::Release);
            return;
        }
        let looking = self.looking.clone();
        let seen = crate::blocking::chat(TICK_DEADLINE, move || {
            // Cleared when the work ends, not when its caller stopped waiting.
            let _done = Done(looking);
            let dirs = agent_dirs();
            jobs.into_iter()
                .map(|job| {
                    let source = job
                        .source
                        .or_else(|| locate(&dirs, &job.program, &job.session));
                    let revision = source
                        .as_ref()
                        .and_then(|s| qoop_parsed::revision_of(s).ok())
                        .map(|r| qoop_parsed::revision_with(r, job.alive, None));
                    Seen {
                        terminal: job.terminal,
                        session: job.session,
                        source,
                        revision,
                    }
                })
                .collect::<Vec<_>>()
        })
        .await;
        let seen = match seen {
            Ok(seen) => seen,
            // The work never started, so nothing will clear the flag.
            Err(
                crate::blocking::BlockingError::Busy
                | crate::blocking::BlockingError::Degraded(_)
                | crate::blocking::BlockingError::Closed,
            ) => {
                self.looking.store(false, Ordering::Release);
                return;
            }
            Err(_) => return,
        };
        self.saw(now, seen);
    }

    /// What a look at `now` found: each watched terminal's file, and the news of a changed one.
    fn saw(&self, now: Instant, seen: Vec<Seen>) {
        let mut watched = self.watched.lock().unwrap();
        for seen in seen {
            let Some(entry) = watched
                .get_mut(&seen.terminal)
                .filter(|w| w.session == seen.session)
            else {
                continue;
            };
            entry.next_locate = seen.source.is_none().then(|| now + RELOCATE);
            entry.source = seen.source;
            // A change inside a second of the last one said keeps the old stamp, so a later
            // tick still finds it changed and says it, with the newest stamp, once the second
            // is up.
            if let Some(revision) = seen.revision
                && entry.revision != Some(revision)
                && entry.told.is_none_or(|at| now >= at + NEWS_EVERY)
            {
                entry.revision = Some(revision);
                entry.told = Some(now);
                // Nobody listening is not an error: nobody is drawing the chat.
                let _ = self.changed.send((seen.terminal, revision));
            }
        }
    }

    /// The task that stats the watched files: every [`TICK`] while there are any, parked
    /// while there are none.
    pub async fn run(
        connections: Arc<crate::daemon::ConnectionRegistry>,
        terminals: Arc<TerminalRegistry>,
        agents: Arc<AgentRegistry>,
    ) {
        let watch = &connections.messages;
        loop {
            if watch.watched.lock().unwrap().is_empty() {
                watch.wake.notified().await;
                continue;
            }
            tokio::time::sleep(TICK).await;
            watch.tick(&terminals, &agents).await;
        }
    }
}

/// Clears the flag of a look that is running.
struct Done(Arc<AtomicBool>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The event a connection's task sends for what the watch broadcast. A missed one is a
/// client that does not know the chat changed, so it is told to read again. `None` is the
/// end of the channel.
pub(crate) fn news(
    said: Result<(u32, u64), tokio::sync::broadcast::error::RecvError>,
) -> Option<EventMessage> {
    use tokio::sync::broadcast::error::RecvError;
    match said {
        Ok((terminal, revision)) => Some(EventMessage::MessagesChanged { terminal, revision }),
        Err(RecvError::Lagged(_)) => Some(EventMessage::Resync),
        Err(RecvError::Closed) => None,
    }
}

fn locate(dirs: &Option<qoop_parsed::Dirs>, program: &str, session: &str) -> Option<Source> {
    qoop_parsed::locate_session(program, session, dirs.as_ref()?)
}

/// What a read asks for.
enum Ask {
    Page {
        child: Option<String>,
        skip: usize,
        limit: usize,
        revision: Option<u64>,
        alive_since: Option<i64>,
    },
    Detail {
        child: Option<String>,
        tool_id: String,
        max_bytes: usize,
    },
}

impl Ask {
    fn child(&self) -> Option<&str> {
        match self {
            Ask::Page { child, .. } | Ask::Detail { child, .. } => child.as_deref(),
        }
    }
}

/// What a read found, whether or not it could answer.
struct Outcome {
    /// Where the file is, when it was found.
    source: Option<Source>,
    /// Its stamp when the read looked.
    revision: Option<u64>,
    memo: Option<Memo>,
    /// The reply, and for a page whether it is the first page of a changed file.
    result: Result<(Arc<Reply>, bool), &'static str>,
}

impl Outcome {
    fn refused(source: Option<Source>, revision: Option<u64>, reason: &'static str) -> Self {
        Outcome {
            source,
            revision,
            memo: None,
            result: Err(reason),
        }
    }
}

/// Read one answer: find the file (unless it is known), stamp it, and reuse the memo when
/// the same was asked at the same stamp.
fn read(
    program: &str,
    session: &str,
    known: Option<Source>,
    memo: Option<Memo>,
    ask: &Ask,
) -> Outcome {
    let dirs = agent_dirs();
    let Some(mut source) = known.or_else(|| locate(&dirs, program, session)) else {
        return Outcome::refused(None, None, "no_session");
    };
    // A subagent's chat: its own file, found from the terminal's and checked to be its child.
    // A page of it also asks the terminal's whether the spawn still runs.
    let mut spawn_ended = None;
    if let Some(child) = ask.child() {
        let codex = dirs.as_ref().map(|d| d.codex.as_path());
        match qoop_parsed::child_source(program, &source, session, child, codex) {
            Some(found) => {
                if let Ask::Page { alive_since, .. } = ask {
                    spawn_ended = qoop_parsed::spawn_ended(&source, child, *alive_since);
                }
                source = found;
            }
            None => return Outcome::refused(None, None, "no_session"),
        }
    }
    let revision = match qoop_parsed::revision_of(&source) {
        Ok(revision) => revision,
        // The file went away since it was found: find it again next time.
        Err(qoop_parsed::Error::NotFound) => return Outcome::refused(None, None, "no_session"),
        Err(e) => return Outcome::refused(Some(source), None, refusal(e)),
    };
    let hit = |key: &Key| {
        memo.as_ref()
            .filter(|m| &m.key == key)
            .map(|m| m.reply.clone())
    };
    match ask {
        Ask::Page {
            child,
            skip,
            limit,
            revision: client,
            alive_since,
        } => {
            // The page is made for the agent that runs now: a new agent in the terminal is
            // another revision of the same file.
            let revision = qoop_parsed::revision_with(revision, *alive_since, spawn_ended);
            // A client whose stamp is not the file's is told to start over: no `skip`.
            let mut skip = if client.is_some_and(|r| r != revision) {
                0
            } else {
                *skip
            };
            let key = |revision, skip| Key {
                revision,
                skip,
                limit: *limit,
                max_bytes: 0,
                tool_id: None,
                alive_since: *alive_since,
                child: child.clone(),
            };
            let load = |skip| {
                let opts = Options {
                    skip,
                    limit: *limit,
                    codex_root: dirs.as_ref().map(|d| d.codex.clone()),
                    alive_since: *alive_since,
                    spawn_ended,
                    ..Options::default()
                };
                qoop_parsed::load(&source, &opts).map(|mut d| {
                    d.revision = qoop_parsed::revision_with(d.revision, *alive_since, spawn_ended);
                    Arc::new(Reply::Page(d))
                })
            };
            let mut reply = match hit(&key(revision, skip)) {
                Some(reply) => reply,
                None => match load(skip) {
                    Ok(reply) => reply,
                    Err(e) => return Outcome::refused(Some(source), Some(revision), refusal(e)),
                },
            };
            let served = |reply: &Reply| match reply {
                Reply::Page(d) => d.revision,
                Reply::Detail(_) => revision,
            };
            // The file grew between the stamp and the parse: `skip` points into something else.
            let reset = client.is_some_and(|r| r != served(&reply));
            if reset && skip > 0 {
                skip = 0;
                reply = match load(0) {
                    Ok(reply) => reply,
                    Err(e) => return Outcome::refused(Some(source), Some(revision), refusal(e)),
                };
            }
            let revision = served(&reply);
            Outcome {
                source: Some(source),
                revision: Some(revision),
                memo: Some(Memo {
                    key: key(revision, skip),
                    reply: reply.clone(),
                }),
                result: Ok((reply, reset)),
            }
        }
        Ask::Detail {
            child,
            tool_id,
            max_bytes,
        } => {
            let key = Key {
                revision,
                skip: 0,
                limit: 0,
                max_bytes: *max_bytes,
                tool_id: Some(tool_id.clone()),
                alive_since: None,
                child: child.clone(),
            };
            if let Some(reply) = hit(&key) {
                return Outcome {
                    source: Some(source),
                    revision: Some(revision),
                    memo: None,
                    result: Ok((reply, false)),
                };
            }
            match qoop_parsed::detail(&source, tool_id, *max_bytes) {
                Ok(detail) => {
                    let reply = Arc::new(Reply::Detail(detail));
                    Outcome {
                        source: Some(source),
                        revision: Some(revision),
                        memo: Some(Memo {
                            key,
                            reply: reply.clone(),
                        }),
                        result: Ok((reply, false)),
                    }
                }
                Err(e) => Outcome::refused(Some(source), Some(revision), refusal(e)),
            }
        }
    }
}

/// Answer a `Messages` or `MessageDetail`. Anything else is not this module's.
pub async fn answer(
    request: ControlRequest,
    terminals: &TerminalRegistry,
    agents: &Arc<AgentRegistry>,
    watch: &Watch,
) -> ControlResponse {
    let page = |child, terminal, skip: u32, limit: u32, revision| Ask::Page {
        child,
        skip: (skip as usize).min(MAX_SKIP),
        limit: if limit == 0 {
            Options::default().limit
        } else {
            limit.min(MAX_LIMIT) as usize
        },
        revision,
        alive_since: alive_since(agents, terminal),
    };
    let detail = |child, tool_id, max_bytes: Option<u32>| Ask::Detail {
        child,
        tool_id,
        max_bytes: max_bytes.unwrap_or(DETAIL_DEFAULT).min(DETAIL_MAX) as usize,
    };
    let (id, terminal, ask) = match request {
        ControlRequest::Messages {
            id,
            terminal,
            skip,
            limit,
            revision,
        } => (id, terminal, page(None, terminal, skip, limit, revision)),
        ControlRequest::SubagentMessages {
            id,
            terminal,
            child,
            skip,
            limit,
            revision,
        } => (
            id,
            terminal,
            page(Some(child), terminal, skip, limit, revision),
        ),
        ControlRequest::MessageDetail {
            id,
            terminal,
            tool_id,
            max_bytes,
        } => (id, terminal, detail(None, tool_id, max_bytes)),
        ControlRequest::SubagentDetail {
            id,
            terminal,
            child,
            tool_id,
            max_bytes,
        } => (id, terminal, detail(Some(child), tool_id, max_bytes)),
        other => return failed(other.request_id().to_string(), "unavailable"),
    };
    let (program, session) = match who(terminals, agents, terminal) {
        Ok(who) => who,
        Err(reason) => return failed(id, reason),
    };
    // At most a few reads of one terminal at once, and they take turns: the second of two
    // alike finds the first one's reply in the memo.
    let Some(ticket) = watch.enter(terminal) else {
        return failed(id, "unavailable");
    };
    let _turn = ticket.turn().await;
    let alive = match &ask {
        Ask::Page { alive_since, .. } => *alive_since,
        Ask::Detail { .. } => None,
    };
    let child = ask.child().is_some();
    let (known, memo) = watch.known(terminal, &session, child);
    let outcome = {
        let session = session.clone();
        crate::blocking::chat(DEADLINE, move || {
            read(&program, &session, known, memo, &ask)
        })
        .await
    };
    let Ok(outcome) = outcome else {
        // Busy, degraded, past its deadline or panicked: the chat is not answering now.
        return failed(id, "unavailable");
    };
    // A subagent's chat keeps the terminal's watch going and nothing else of it.
    if child {
        watch.renew(terminal, &session, outcome.memo);
    // Worth hearing about when it changes: a read that answered, or found no file yet.
    } else if outcome.result.is_ok() || matches!(outcome.result, Err("no_session")) {
        watch.mark(
            terminal,
            &session,
            outcome.source,
            outcome.revision,
            outcome.memo,
            alive,
        );
    }
    match outcome.result {
        Ok((reply, reset)) => match &*reply {
            Reply::Page(digest) => ControlResponse::Messages {
                id,
                revision: digest.revision,
                reset,
                first_seq: digest.first_seq,
                has_older: digest.has_older,
                items: digest.items.clone(),
                warnings: digest.warnings,
            },
            Reply::Detail(detail) => ControlResponse::MessageDetail {
                id,
                input: detail.input.clone(),
                output: detail.output.clone(),
                truncated: detail.truncated,
            },
        },
        Err(reason) => failed(id, reason),
    }
}

/// One line at a time into the leads' terminals: the words and their Enter are two writes with
/// a pause between, and another line landing in the pause would be sent glued to this one.
/// ponytail: one lock for every terminal, per-terminal locks if sends ever queue behind it.
static TYPING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The longest name of a child that is put in a line.
const NAME_MAX: usize = 60;

/// What the lead calls the child: the `name` its `agent-<child>.meta.json` beside the child's
/// session carries (a teammate's own name), else `subagent <child>`. Cleaned to one line, as it
/// is typed into a prompt.
fn child_label(source: &Source, child: &str) -> String {
    let named = match source {
        Source::File(path) => std::fs::read(path.with_extension("meta.json"))
            .ok()
            .filter(|bytes| bytes.len() <= 64 * 1024)
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|meta| meta["name"].as_str().map(qoop_store::task::one_line)),
        Source::Opencode { .. } => None,
    };
    named
        .map(|name| name.chars().take(NAME_MAX).collect::<String>())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("subagent {child}"))
}

/// Answer a `SubagentSend`: type `Tell <name>: <text>` and an Enter into the lead's terminal,
/// the one way to reach a subagent (the lead passes it on). The reason codes are the ones
/// `ControlRequest::SubagentSend` lists; nothing is typed when one is given.
pub async fn send(
    id: String,
    terminal: u32,
    child: String,
    text: String,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
) -> ControlResponse {
    let Some(text) = crate::asks::clean_text(&text) else {
        return failed(id, "empty");
    };
    if text.chars().count() > crate::asks::TEXT_MAX {
        return failed(id, "too_long");
    }
    let (program, session) = match who(terminals, agents, terminal) {
        Ok(who) => who,
        Err(reason) => return failed(id, reason),
    };
    // The child must be one of this terminal's own session, found the way its chat is.
    let found = crate::blocking::chat(DEADLINE, move || {
        let dirs = agent_dirs();
        let source = locate(&dirs, &program, &session)?;
        let codex = dirs.as_ref().map(|d| d.codex.as_path());
        let source = qoop_parsed::child_source(&program, &source, &session, &child, codex)?;
        Some(child_label(&source, &child))
    })
    .await;
    let Ok(Some(to)) = found else {
        return failed(id, "no_session");
    };
    // A person at the keyboard keeps it: their draft would be sent glued to this line.
    if agents
        .person_until(terminal, crate::agents::now_unix())
        .is_some()
    {
        return failed(id, "person_typing");
    }
    // A working lead takes it as a queued prompt; a dialog or a sleeping tab would not read it.
    if let Some(reason) = crate::asks::standing(agents, terminals, terminal)
        .and_then(|s| crate::asks::refusal(&s.state, &s.readiness))
        .filter(|reason| *reason != "working")
    {
        return failed(id, reason);
    }
    let _one_at_a_time = TYPING.lock().await;
    match crate::asks::type_into(terminals, terminal, &format!("Tell {to}: {text}")).await {
        Ok(()) => ControlResponse::SubagentSent { id, to },
        Err(_) => failed(id, "unavailable"),
    }
}

/// A short reason code for a refusal, the only thing a client branches on.
fn refusal(e: qoop_parsed::Error) -> &'static str {
    match e {
        qoop_parsed::Error::TooLarge => "too_large",
        qoop_parsed::Error::NotFound => "no_session",
        qoop_parsed::Error::Io(_) | qoop_parsed::Error::Parse(_) => "unavailable",
    }
}

fn failed(id: String, reason: &str) -> ControlResponse {
    ControlResponse::Failed {
        id,
        reason: reason.to_string(),
    }
}

/// The program and the conversation of the agent in a terminal, from the daemon's own
/// registry, or the reason code for why there are none.
fn who(
    terminals: &TerminalRegistry,
    agents: &Arc<AgentRegistry>,
    terminal: u32,
) -> Result<(String, String), &'static str> {
    if terminals.get(terminal).is_none() {
        return Err("unavailable");
    }
    let program = agent_program(agents, terminal).ok_or("not_agent")?;
    let session = terminals.session_of(terminal).ok_or("no_session")?;
    Ok((program, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(session: &'static str) -> impl Fn(u32) -> Option<(String, String)> {
        move |_| Some(("claude".to_string(), session.to_string()))
    }

    #[test]
    fn every_failure_of_the_parser_is_one_of_the_four_reason_codes() {
        use qoop_parsed::Error;
        assert_eq!(refusal(Error::TooLarge), "too_large");
        assert_eq!(refusal(Error::NotFound), "no_session");
        assert_eq!(
            refusal(Error::Io("permission denied".into())),
            "unavailable"
        );
        assert_eq!(refusal(Error::Parse("not a session".into())), "unavailable");
    }

    /// The stamp a watch starts from is the one first served, so a second asker, which was
    /// served a later one, does not hide the change in between from the first.
    #[test]
    fn a_watch_keeps_the_first_stamp_and_starts_over_for_another_conversation() {
        let watch = Watch::default();
        watch.mark(4, "one", None, Some(10), None, None);
        watch.mark(4, "one", None, Some(11), None, None);
        assert_eq!(watch.watched.lock().unwrap()[&4].revision, Some(10));
        watch.mark(4, "two", None, Some(12), None, None);
        let held = watch.watched.lock().unwrap();
        assert_eq!(held[&4].revision, Some(12));
        assert_eq!(held[&4].session, "two");
    }

    #[test]
    fn a_subagent_read_renews_the_watch_and_keeps_its_reply_apart() {
        let watch = Watch::default();
        let memo = |child: Option<&str>| Memo {
            key: Key {
                revision: 1,
                skip: 0,
                limit: 1,
                max_bytes: 0,
                tool_id: None,
                alive_since: None,
                child: child.map(String::from),
            },
            reply: Arc::new(Reply::Detail(qoop_parsed::Detail {
                input: String::new(),
                output: String::new(),
                truncated: false,
            })),
        };
        let own = Source::File("/p/s.jsonl".into());
        watch.mark(4, "one", Some(own.clone()), Some(1), Some(memo(None)), None);
        let before = watch.watched.lock().unwrap()[&4].asked;
        watch.renew(4, "one", Some(memo(Some("c"))));
        // Another conversation's read keeps nothing.
        watch.renew(4, "two", Some(memo(Some("x"))));
        let (source, parent) = watch.known(4, "one", false);
        let (_, child) = watch.known(4, "one", true);
        assert_eq!(source, Some(own));
        assert_eq!(parent.unwrap().key.child, None);
        assert_eq!(child.unwrap().key.child.as_deref(), Some("c"));
        assert!(watch.watched.lock().unwrap()[&4].asked >= before);
    }

    #[test]
    fn a_watch_expires_thirty_seconds_after_the_last_ask() {
        let watch = Watch::default();
        watch.mark(4, "one", None, Some(1), None, None);
        let now = Instant::now();
        watch.due_with(now + Duration::from_secs(29), agent("one"));
        assert!(watch.watched.lock().unwrap().contains_key(&4));
        assert!(
            watch
                .due_with(now + Duration::from_secs(31), agent("one"))
                .is_empty()
        );
        assert!(
            watch.watched.lock().unwrap().is_empty(),
            "an expired watch stays in the map"
        );
    }

    #[test]
    fn a_watch_does_not_outlive_its_terminal_its_agent_or_its_conversation() {
        let watch = Watch::default();
        for terminal in 1..=3 {
            watch.mark(terminal, "one", None, Some(1), None, None);
        }
        let now = Instant::now();
        // Terminal 1 closed, terminal 2 is a shell now, terminal 3 is in another conversation.
        let jobs = watch.due_with(now, |terminal| match terminal {
            1 | 2 => None,
            _ => Some(("claude".into(), "other".into())),
        });
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].session, "other");
        assert!(jobs[0].source.is_none());
        let held = watch.watched.lock().unwrap();
        assert_eq!(held.keys().copied().collect::<Vec<_>>(), vec![3]);
        // And what was known of the old conversation is gone with it.
        assert_eq!(held[&3].revision, None);
        assert!(held[&3].memo.is_none());
    }

    /// The watch stamps the file the way the last page was stamped, so a page made for an agent
    /// is not "changed" by the first tick, and a detail read does not unsay the agent.
    #[test]
    fn the_watch_looks_with_the_agent_the_last_page_was_made_for() {
        let watch = Watch::default();
        watch.mark(4, "one", None, Some(1), None, Some(5_000));
        watch.mark(4, "one", None, None, None, None);
        let later = Instant::now() + Duration::from_secs(6);
        let jobs = watch.due_with(later, agent("one"));
        assert_eq!(jobs[0].alive, Some(5_000));
    }

    #[test]
    fn a_file_that_is_not_there_is_looked_for_every_five_seconds_not_every_tick() {
        let watch = Watch::default();
        watch.mark(4, "one", None, None, None, None);
        let now = Instant::now();
        assert!(
            watch
                .due_with(now + Duration::from_secs(1), agent("one"))
                .is_empty()
        );
        assert!(
            watch
                .due_with(now + Duration::from_secs(4), agent("one"))
                .is_empty()
        );
        assert_eq!(
            watch
                .due_with(now + Duration::from_secs(6), agent("one"))
                .len(),
            1
        );
    }

    /// The most reads of one terminal that are in the daemon: the fifth is refused at once, and
    /// a finished one makes room.
    #[test]
    fn a_terminal_has_a_few_reads_in_flight_and_no_more() {
        let watch = Watch::default();
        let tickets: Vec<_> = (0..MAX_QUEUED).map(|_| watch.enter(9).unwrap()).collect();
        assert!(watch.enter(9).is_none());
        assert!(watch.enter(10).is_some(), "another terminal is not held up");
        drop(tickets);
        assert!(watch.enter(9).is_some());
        assert!(
            watch.flights.lock().unwrap().is_empty(),
            "a turn lock is left behind"
        );
    }

    /// A change is said at the first look that sees it; the next waits a second, then goes with
    /// the newest stamp, so a growing file sends its readers back once a second at most.
    #[test]
    fn a_change_is_said_at_once_and_then_at_most_once_a_second() {
        let watch = Watch::default();
        let mut heard = watch.subscribe();
        watch.mark(4, "one", None, Some(1), None, None);
        let look = |revision| {
            vec![Seen {
                terminal: 4,
                session: "one".into(),
                source: None,
                revision: Some(revision),
            }]
        };
        let now = Instant::now();
        watch.saw(now, look(1));
        assert!(
            heard.try_recv().is_err(),
            "nothing changed, nothing is said"
        );
        watch.saw(now + Duration::from_millis(200), look(2));
        assert_eq!(heard.try_recv().ok(), Some((4, 2)));
        watch.saw(now + Duration::from_millis(400), look(3));
        watch.saw(now + Duration::from_millis(1000), look(4));
        assert!(heard.try_recv().is_err(), "said again inside the second");
        watch.saw(now + Duration::from_millis(1200), look(5));
        assert_eq!(heard.try_recv().ok(), Some((4, 5)));
        assert!(heard.try_recv().is_err());
    }

    /// A client that missed news is told to read again; the channel closing ends the task.
    #[tokio::test]
    async fn news_missed_is_a_resync_and_news_heard_is_the_change() {
        let watch = Watch::default();
        let mut heard = watch.subscribe();
        for n in 0..40 {
            let _ = watch.changed.send((1, n));
        }
        assert_eq!(news(heard.recv().await), Some(EventMessage::Resync));
        assert_eq!(
            news(heard.recv().await),
            Some(EventMessage::MessagesChanged {
                terminal: 1,
                revision: 24
            })
        );
        assert_eq!(
            news(Err(tokio::sync::broadcast::error::RecvError::Closed)),
            None
        );
    }

    fn line(kind: &str, n: usize, message: serde_json::Value) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": kind, "uuid": format!("u{n}"), "sessionId": "s1",
                "timestamp": "2026-10-12T09:00:00.000Z", "message": message})
        )
    }

    /// The same read of a file that has not moved is the memo's reply, not another parse; a
    /// file that grew gets a new one, and a detail read is kept the same way.
    #[test]
    fn a_read_of_an_unchanged_file_is_answered_from_the_memo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        let mut text = String::new();
        for n in 0..6 {
            text.push_str(&line(
                "user",
                n,
                serde_json::json!({"role": "user", "content": format!("q{n}")}),
            ));
        }
        std::fs::write(&path, &text).unwrap();
        let source = Source::File(path.clone());
        let page = Ask::Page {
            child: None,
            alive_since: None,
            skip: 0,
            limit: 3,
            revision: None,
        };
        let first = read("claude", "s1", Some(source.clone()), None, &page);
        let (reply, reset) = first.result.as_ref().expect("a page").clone();
        assert!(!reset);
        let again = read(
            "claude",
            "s1",
            Some(source.clone()),
            first.memo.clone(),
            &page,
        );
        assert!(
            Arc::ptr_eq(&reply, &again.result.as_ref().ok().unwrap().0),
            "parsed twice"
        );
        // Another page is another reply.
        let deeper = Ask::Page {
            child: None,
            alive_since: None,
            skip: 1,
            limit: 3,
            revision: None,
        };
        let other = read(
            "claude",
            "s1",
            Some(source.clone()),
            first.memo.clone(),
            &deeper,
        );
        assert!(!Arc::ptr_eq(&reply, &other.result.as_ref().ok().unwrap().0));
        // The file grew: the memo is not good any more.
        std::thread::sleep(Duration::from_millis(20));
        let more = line(
            "user",
            9,
            serde_json::json!({"role": "user", "content": "late"}),
        );
        std::fs::write(&path, format!("{text}{more}")).unwrap();
        let grown = read("claude", "s1", Some(source), first.memo, &page);
        let (new, _) = grown.result.as_ref().ok().unwrap();
        assert!(!Arc::ptr_eq(&reply, new));
        assert_ne!(grown.revision, first.revision);
    }

    /// A page is made for the agent that runs now. The same file read for another agent is
    /// another reply and another revision, never the memo's, so a client never holds pages of
    /// two agents under one revision; the same agent reads the same.
    #[test]
    fn a_page_made_for_another_agent_is_not_served_from_the_memo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(
            &path,
            line(
                "user",
                0,
                serde_json::json!({"role": "user", "content": "q"}),
            ),
        )
        .unwrap();
        let source = Source::File(path);
        let page = |alive_since| Ask::Page {
            child: None,
            alive_since,
            skip: 0,
            limit: 3,
            revision: None,
        };
        let first = read(
            "claude",
            "s1",
            Some(source.clone()),
            None,
            &page(Some(1_000)),
        );
        let same = read(
            "claude",
            "s1",
            Some(source.clone()),
            first.memo.clone(),
            &page(Some(1_000)),
        );
        assert!(Arc::ptr_eq(
            &first.result.as_ref().ok().unwrap().0,
            &same.result.as_ref().ok().unwrap().0
        ));
        assert_eq!(same.revision, first.revision);
        let other = read(
            "claude",
            "s1",
            Some(source.clone()),
            first.memo.clone(),
            &page(Some(2_000)),
        );
        assert!(!Arc::ptr_eq(
            &first.result.as_ref().ok().unwrap().0,
            &other.result.as_ref().ok().unwrap().0
        ));
        assert_ne!(other.revision, first.revision);
        // A client that holds the first agent's revision is told to start over.
        let held = Ask::Page {
            child: None,
            alive_since: Some(2_000),
            skip: 1,
            limit: 3,
            revision: first.revision,
        };
        let again = read("claude", "s1", Some(source), None, &held);
        assert!(again.result.as_ref().is_ok());
        let digest = |o: &Outcome| match &*o.result.as_ref().ok().unwrap().0 {
            Reply::Page(d) => d.revision,
            Reply::Detail(_) => 0,
        };
        assert_eq!(digest(&again), digest(&other), "served at the new revision");
    }

    /// The real shape (crw-saas `72d85121`, call `toolu_017C5gbi2h42T9bmxBZFuQgk`): a subagent
    /// launches a background Bash, and Claude writes its end to the parent file only. While the
    /// spawn runs the call runs; once the spawn has ended, the call that has its launch text
    /// reads ok and one with no result interrupted, and the page is another revision though the
    /// subagent's file did not change.
    /// One Claude record of session `s1` at minute `n`, with `extra` merged in.
    fn rec(kind: &str, n: usize, message: serde_json::Value, extra: serde_json::Value) -> String {
        let mut v = serde_json::json!({"type": kind, "uuid": format!("u{n}"),
            "sessionId": "s1", "timestamp": format!("2026-08-07T01:{n:02}:00.000Z"),
            "message": message});
        if let (Some(v), serde_json::Value::Object(extra)) = (v.as_object_mut(), extra) {
            v.extend(extra);
        }
        format!("{v}\n")
    }

    /// The tool rows of a page: summary and status.
    fn tool_statuses(o: &Outcome) -> Vec<(String, qoop_parsed::Status)> {
        match &*o.result.as_ref().expect("a page").0 {
            Reply::Page(d) => d
                .items
                .iter()
                .filter_map(|i| match i {
                    qoop_parsed::Item::Tool {
                        summary, status, ..
                    } => Some((summary.clone(), *status)),
                    _ => None,
                })
                .collect(),
            Reply::Detail(_) => Vec::new(),
        }
    }

    #[test]
    fn a_subagents_rows_end_with_its_spawn() {
        let child_id = "afactchecker-fe8b007d8456da6b";
        let none = serde_json::Value::Null;
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("s1.jsonl");
        let spawned = [
            rec("user", 1, serde_json::json!({"role": "user", "content": "check the site"}), none.clone()),
            rec("assistant", 2, serde_json::json!({"id": "m1", "role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_spawn", "name": "Agent", "input": {
                    "description": "Fact check", "prompt": "check", "run_in_background": true}}]}), none.clone()),
            rec("user", 3, serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_spawn", "content":
                    format!("Async agent launched successfully.\nagentId: {child_id} (internal ID - do not mention to user.)")}]}),
                serde_json::json!({"toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": child_id}})),
        ]
        .concat();
        std::fs::write(&parent, &spawned).unwrap();
        let subagents = dir.path().join("s1/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            subagents.join(format!("agent-{child_id}.jsonl")),
            [
                rec("assistant", 4, serde_json::json!({"id": "m2", "role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_017C5gbi2h42T9bmxBZFuQgk", "name": "Bash", "input": {
                        "command": "./rc/crawl.sh", "description": "Crawl all sitemap pages",
                        "run_in_background": true}}]}), serde_json::json!({"isSidechain": true, "agentId": child_id})),
                rec("user", 5, serde_json::json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_017C5gbi2h42T9bmxBZFuQgk", "is_error": false, "content":
                        "Command running in background with ID: bdkljd5e7. Output is being written to: /x/tasks/bdkljd5e7.output. You will be notified when it completes. To check interim output, use Read on that file path."}]}),
                    serde_json::json!({"isSidechain": true, "agentId": child_id})),
                rec("assistant", 6, serde_json::json!({"id": "m3", "role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_noresult", "name": "Bash", "input": {"command": "sleep 600"}}]}),
                    serde_json::json!({"isSidechain": true, "agentId": child_id})),
            ]
            .concat(),
        )
        .unwrap();
        let page = Ask::Page {
            child: Some(child_id.into()),
            alive_since: None,
            skip: 0,
            limit: 10,
            revision: None,
        };
        let statuses = tool_statuses;
        use qoop_parsed::Status::{Interrupted, Ok, Running};
        let source = Source::File(parent.clone());
        let running = read("claude", "s1", Some(source.clone()), None, &page);
        assert_eq!(
            statuses(&running),
            [
                ("./rc/crawl.sh".to_string(), Running),
                ("sleep 600".to_string(), Running)
            ],
            "the spawn still runs"
        );
        // The parent hears the agent finish; the subagent's file does not change.
        std::fs::write(
            &parent,
            format!(
                "{spawned}{}",
                rec("user", 7, serde_json::json!({"role": "user", "content":
                    "<task-notification>\n<task-id>afact</task-id>\n<tool-use-id>toolu_spawn</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Fact check\" completed</summary>\n<result>done</result>\n</task-notification>"}),
                    none.clone())
            ),
        )
        .unwrap();
        let ended = read("claude", "s1", Some(source), running.memo.clone(), &page);
        assert_eq!(
            statuses(&ended),
            [
                ("./rc/crawl.sh".to_string(), Ok),
                ("sleep 600".to_string(), Interrupted)
            ],
            "the spawn has ended"
        );
        assert_ne!(ended.revision, running.revision);
        // The parent goes on past the newest 1 MiB: the spawn's end is still found.
        let filler: String = (10..30)
            .map(|n| {
                rec(
                    "user",
                    n,
                    serde_json::json!({"role": "user", "content": "x".repeat(64 * 1024)}),
                    none.clone(),
                )
            })
            .collect();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap();
        std::io::Write::write_all(&mut file, filler.as_bytes()).unwrap();
        let older = read(
            "claude",
            "s1",
            Some(Source::File(parent.clone())),
            ended.memo,
            &page,
        );
        assert_eq!(
            statuses(&older),
            [
                ("./rc/crawl.sh".to_string(), Ok),
                ("sleep 600".to_string(), Interrupted)
            ],
            "the spawn has ended, older than the newest 1 MiB of the parent"
        );
    }

    /// Claude resumes a stopped subagent with `SendMessage`, and the resumed run writes to the
    /// same file: a call it makes after the spawn ended runs, one from before it does not.
    #[test]
    fn a_resumed_subagents_new_calls_still_run() {
        let child_id = "a6659747624983776";
        let none = serde_json::Value::Null;
        let side = serde_json::json!({"isSidechain": true, "agentId": child_id});
        let call = |n, id: &str, name: &str, input: serde_json::Value, extra: serde_json::Value| {
            rec(
                "assistant",
                n,
                serde_json::json!({"id": format!("m{n}"), "role": "assistant",
                "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
                extra,
            )
        };
        let result = |n, id: &str, text: &str, extra: serde_json::Value| {
            rec(
                "user",
                n,
                serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": text}]}),
                extra,
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("s1.jsonl");
        std::fs::write(&parent, [
            rec("user", 1, serde_json::json!({"role": "user", "content": "fix it"}), none.clone()),
            call(2, "toolu_spawn", "Agent", serde_json::json!({"description": "fix",
                "prompt": "fix", "subagent_type": "general-purpose", "run_in_background": true}),
                none.clone()),
            result(3, "toolu_spawn", &format!("Async agent launched successfully.\nagentId: {child_id} (internal ID - do not mention to user.)"),
                serde_json::json!({"toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": child_id}})),
            rec("user", 7, serde_json::json!({"role": "user", "content": format!(
                "<task-notification>\n<task-id>{child_id}</task-id>\n<tool-use-id>toolu_spawn</tool-use-id>\n<status>failed</status>\n<summary>Agent \"fix\" failed</summary>\n</task-notification>")}),
                none.clone()),
            call(20, "toolu_send", "SendMessage", serde_json::json!({"to": child_id,
                "summary": "Resume", "message": "continue"}), none.clone()),
            result(21, "toolu_send", &format!("{{\"success\":true,\"message\":\"Resuming agent a665974\",\"resumedAgentId\":\"{child_id}\"}}"),
                none.clone()),
        ].concat()).unwrap();
        let subagents = dir.path().join("s1/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            subagents.join(format!("agent-{child_id}.jsonl")),
            [
                rec(
                    "user",
                    2,
                    serde_json::json!({"role": "user", "content": "fix"}),
                    side.clone(),
                ),
                call(
                    4,
                    "toolu_old",
                    "Bash",
                    serde_json::json!({"command": "cargo build"}),
                    side.clone(),
                ),
                result(5, "toolu_old", "ok", side.clone()),
                rec(
                    "user",
                    21,
                    serde_json::json!({"role": "user", "content": "continue"}),
                    side.clone(),
                ),
                call(
                    22,
                    "toolu_now",
                    "Bash",
                    serde_json::json!({"command": "cargo test"}),
                    side.clone(),
                ),
            ]
            .concat(),
        )
        .unwrap();
        let page = Ask::Page {
            child: Some(child_id.into()),
            alive_since: None,
            skip: 0,
            limit: 10,
            revision: None,
        };
        let out = read("claude", "s1", Some(Source::File(parent)), None, &page);
        use qoop_parsed::Status::{Ok, Running};
        assert_eq!(
            tool_statuses(&out),
            [
                ("cargo build".to_string(), Ok),
                ("cargo test".to_string(), Running)
            ]
        );
    }

    #[test]
    fn a_session_that_is_not_there_is_no_session_and_remembers_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let gone = Source::File(dir.path().join("missing.jsonl"));
        let page = Ask::Page {
            child: None,
            alive_since: None,
            skip: 0,
            limit: 3,
            revision: None,
        };
        let out = read("claude", "s1", Some(gone), None, &page);
        assert_eq!(out.result.err(), Some("no_session"));
        assert!(out.source.is_none() && out.revision.is_none());
    }
}
