//! The chat of an agent terminal: `Messages`, `MessageDetail` and `MessagesChanged`.
//!
//! The daemon owns the disk the agent writes its session to, so it parses (`qoop_parsed`)
//! and a client, local or remote, only draws. A read is a tail parse of a few to a few tens of
//! milliseconds, on the chat pool (two threads, see `blocking::chat`) and never on the read
//! loop. A flood costs two threads and a refusal each: no permit free answers `unavailable`
//! at once, a terminal has at most [`MAX_QUEUED`] reads waiting on its turn, and the last
//! page of a watched terminal is kept while its file is unchanged.
//!
//! No file watcher exists, so a terminal a client asked about is stat-ed once a second for
//! 30 seconds after its last question, and a changed stamp is said as an event. With nobody
//! asking the task is parked and the daemon wakes for nothing.
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

/// How often a watched terminal's session file is looked at.
const TICK: Duration = Duration::from_secs(1);

/// How often a watched terminal whose file is not there yet is looked for.
const RELOCATE: Duration = Duration::from_secs(5);

/// What a disk is given to answer a client's read, and the watch's look.
const DEADLINE: Duration = Duration::from_secs(5);
const TICK_DEADLINE: Duration = Duration::from_secs(2);

/// The most items one page may ask for; the page's own byte budget cuts it shorter.
const MAX_LIMIT: u32 = 200;

/// The default and the largest cut of one side of a tool call.
const DETAIL_DEFAULT: u32 = 64 * 1024;
const DETAIL_MAX: u32 = 1024 * 1024;

/// The most reads of one terminal in the daemon at once, the one running included. More are
/// refused at once: a client that asks faster than a parse is not waited for.
const MAX_QUEUED: usize = 4;

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
    /// every second.
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

    /// What is known of a terminal's chat, to save a read the work.
    fn known(&self, terminal: u32, session: &str) -> (Option<Source>, Option<Memo>) {
        match self.watched.lock().unwrap().get(&terminal) {
            Some(w) if w.session == session => (w.source.clone(), w.memo.clone()),
            _ => (None, None),
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
    ) {
        let mut watched = self.watched.lock().unwrap();
        let entry = watched.entry(terminal).or_insert_with(|| Watched {
            asked: Instant::now(),
            session: session.to_string(),
            source: None,
            revision,
            next_locate: None,
            memo: None,
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
            };
        }
        entry.next_locate = source.is_none().then(|| Instant::now() + RELOCATE);
        entry.source = source;
        if memo.is_some() {
            entry.memo = memo;
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
                        .and_then(|s| qoop_parsed::revision_of(s).ok());
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
            if let Some(revision) = seen.revision
                && entry.revision != Some(revision)
            {
                entry.revision = Some(revision);
                // Nobody listening is not an error: nobody is drawing the chat.
                let _ = self.changed.send((seen.terminal, revision));
            }
        }
    }

    /// The task that stats the watched files: once a second while there are any, parked
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
        skip: usize,
        limit: usize,
        revision: Option<u64>,
    },
    Detail {
        tool_id: String,
        max_bytes: usize,
    },
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
    let Some(source) = known.or_else(|| locate(&agent_dirs(), program, session)) else {
        return Outcome::refused(None, None, "no_session");
    };
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
            skip,
            limit,
            revision: client,
        } => {
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
            };
            let load = |skip| {
                let opts = Options {
                    skip,
                    limit: *limit,
                    codex_root: agent_dirs().map(|d| d.codex),
                    ..Options::default()
                };
                qoop_parsed::load(&source, &opts).map(|d| Arc::new(Reply::Page(d)))
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
        Ask::Detail { tool_id, max_bytes } => {
            let key = Key {
                revision,
                skip: 0,
                limit: 0,
                max_bytes: *max_bytes,
                tool_id: Some(tool_id.clone()),
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
    let (id, terminal, ask) = match request {
        ControlRequest::Messages {
            id,
            terminal,
            skip,
            limit,
            revision,
        } => {
            let limit = if limit == 0 {
                Options::default().limit
            } else {
                limit.min(MAX_LIMIT) as usize
            };
            let skip = (skip as usize).min(MAX_SKIP);
            (
                id,
                terminal,
                Ask::Page {
                    skip,
                    limit,
                    revision,
                },
            )
        }
        ControlRequest::MessageDetail {
            id,
            terminal,
            tool_id,
            max_bytes,
        } => {
            let max_bytes = max_bytes.unwrap_or(DETAIL_DEFAULT).min(DETAIL_MAX) as usize;
            (id, terminal, Ask::Detail { tool_id, max_bytes })
        }
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
    let (known, memo) = watch.known(terminal, &session);
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
    // Worth hearing about when it changes: a read that answered, or found no file yet.
    if outcome.result.is_ok() || matches!(outcome.result, Err("no_session")) {
        watch.mark(
            terminal,
            &session,
            outcome.source,
            outcome.revision,
            outcome.memo,
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
        watch.mark(4, "one", None, Some(10), None);
        watch.mark(4, "one", None, Some(11), None);
        assert_eq!(watch.watched.lock().unwrap()[&4].revision, Some(10));
        watch.mark(4, "two", None, Some(12), None);
        let held = watch.watched.lock().unwrap();
        assert_eq!(held[&4].revision, Some(12));
        assert_eq!(held[&4].session, "two");
    }

    #[test]
    fn a_watch_expires_thirty_seconds_after_the_last_ask() {
        let watch = Watch::default();
        watch.mark(4, "one", None, Some(1), None);
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
            watch.mark(terminal, "one", None, Some(1), None);
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

    #[test]
    fn a_file_that_is_not_there_is_looked_for_every_five_seconds_not_every_tick() {
        let watch = Watch::default();
        watch.mark(4, "one", None, None, None);
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

    #[test]
    fn a_session_that_is_not_there_is_no_session_and_remembers_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let gone = Source::File(dir.path().join("missing.jsonl"));
        let page = Ask::Page {
            skip: 0,
            limit: 3,
            revision: None,
        };
        let out = read("claude", "s1", Some(gone), None, &page);
        assert_eq!(out.result.err(), Some("no_session"));
        assert!(out.source.is_none() && out.revision.is_none());
    }
}
