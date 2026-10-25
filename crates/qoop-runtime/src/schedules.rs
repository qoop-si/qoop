//! The daemon's side of scheduled agents: the writes and the loop that fires them.
//!
//! The rows are in `qoop.db` (`qoop-store`'s `schedule.rs`) and the timing is in `schedule.rs`
//! beside this file. What is here is what needs the daemon:
//!
//! - the writes (`qoop schedule`, the app), refused from a worker's terminal as a card is, since
//!   a worker that follows a prompt to schedule something would otherwise start agents in the
//!   owner's name on its own;
//! - the loop, which looks at every row, claims the slots that are due (one compare and set in
//!   the store, so a slot is fired once whatever restarts or handovers happen around it) and
//!   starts `qoop schedule fire` for each, a child of its own that opens the terminal and
//!   reports back over the socket;
//! - the two numbers the rest of the daemon reads: whether a schedule is active, which keeps the
//!   daemon from leaving, and how many runs are starting, which keeps it from moving onto a new
//!   binary in the middle of one.
//!
//! The loop costs nothing while no schedule is active: it parks on the schedules broadcast.
//! While one is, it sleeps until the nearest slot, and never longer than a minute, because the
//! tokio clock does not count the time a Mac spends asleep: a wake or a clock change is seen
//! within a minute whatever the sleep was meant to be.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use qoop_store::ScheduleRow;

use crate::agents::AgentRegistry;
use crate::asks::{Caller, QASS_SPACE, failed};
use crate::control::{
    ControlResponse, SCHEDULE_PROMPT_MAX, SCHEDULES_MOST, SEEN_MOST, ScheduleInfo, ScheduleOp,
};
use crate::daemon::{Bookkeeping, ConnectionRegistry, PROBE_DEADLINE};
use crate::schedule::{Cron, Decision, Zone, decide, next_at, system_zone, when_words};
use crate::store::{HandleError, now};

/// The longest the loop sleeps without looking, so a wake from sleep or a clock set to another
/// time is seen within this.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// How long a due slot waits for a restart's gate to come down, or a store that did not answer
/// to answer, before the loop looks again.
const RETRY: Duration = Duration::from_secs(2);

/// How long a run has to open its terminal and report: the shell it types into gets ten
/// seconds to draw (`open_terminal`), and the rest is the daemon being slow.
const RUN_PATIENCE: Duration = Duration::from_secs(60);

/// The longest agent name a schedule may carry.
const AGENT_MOST: usize = 40;

/// The longest space name.
const SPACE_MOST: usize = 60;

/// The longest command a watch runs.
const WATCH_MOST: usize = 2000;

/// What the rest of the daemon reads of the schedules.
#[derive(Default)]
pub struct Schedules {
    /// How many schedules are active: not paused and not done. The daemon does not leave
    /// while one is, since nothing else would start the agent at its time.
    active: AtomicUsize,
    /// How many runs have been claimed or asked for and have not ended: a move onto a new
    /// binary waits for them, since its child would find the daemon gone half way.
    in_flight: AtomicUsize,
}

impl Schedules {
    /// Whether a schedule is waiting for its time, or a run is starting.
    pub fn busy(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0 || self.in_flight() > 0
    }

    /// How many runs are starting.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

/// One run starting, counted for as long as it is held.
///
/// Taken before the check that no restart is arming, so that either the loop sees the gate and
/// lets go, or the restart sees the run and waits: neither can look past the other.
struct Starting(Arc<Schedules>);

impl Starting {
    fn new(schedules: &Arc<Schedules>) -> Self {
        schedules.in_flight.fetch_add(1, Ordering::SeqCst);
        Self(schedules.clone())
    }
}

impl Drop for Starting {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A store refusal as a sentence: a schedule refusal already is one.
fn why(error: HandleError) -> String {
    match error {
        HandleError::Timeout => "qoop did not confirm in time; check qoop schedule ls".to_string(),
        HandleError::Store(e) => e.to_string(),
    }
}

/// Whether a run may be claimed or started now: not while the daemon is going, and not while a
/// move onto a new binary is ending things.
fn closed(books: &Bookkeeping) -> bool {
    books.stopping.load(Ordering::SeqCst) || books.restart.gated()
}

/// Tell what the daemon's leaving waits on that the count changed.
fn set_active(books: &Bookkeeping, connections: &ConnectionRegistry, active: usize) {
    if books.schedules.active.swap(active, Ordering::SeqCst) != active {
        connections.wake_leaving();
    }
}

/// Count the active schedules again, after a write: so an add during the countdown to leaving
/// keeps the daemon, and the last remove lets it go, without waiting for the loop's next look.
async fn recount(books: &Bookkeeping, connections: &ConnectionRegistry) {
    let zone = system_zone();
    let at = now();
    if let Ok(rows) = books.store.run(|s| s.schedules()).await {
        let active = rows
            .iter()
            .filter(|row| decide(row, at, zone) != Decision::Idle)
            .count();
        set_active(books, connections, active);
    }
}

fn info(row: ScheduleRow, at: i64, zone: Zone) -> ScheduleInfo {
    ScheduleInfo {
        next_at: next_at(&row, at, zone),
        when: when_words(&row, zone),
        id: row.id,
        name: row.name,
        cron: row.cron,
        at: row.at,
        watch: row.watch,
        agent: row.agent,
        cwd: row.cwd,
        space: row.space,
        prompt: row.prompt,
        paused: row.paused,
        created_at: row.created_at,
        last_slot: row.last_slot,
        last_at: row.last_at,
        last_result: row.last_result,
        last_terminal: row.last_terminal,
    }
}

pub(crate) async fn read(id: String, books: &Bookkeeping) -> ControlResponse {
    match books.store.run(|s| s.schedules()).await {
        Ok(rows) => {
            let (at, zone) = (now(), system_zone());
            ControlResponse::Schedules {
                id,
                schedules: rows.into_iter().map(|row| info(row, at, zone)).collect(),
            }
        }
        Err(e) => failed(id, why(e)),
    }
}

/// Whether the caller may write schedules: a client that speaks for no terminal (the app, the
/// CLI outside qoop, the run reporting), or a terminal of qass's own space with its token. A
/// worker's terminal is refused, as is a half of a pair. Like `shows.rs` says of its token,
/// this stops accidents and not a local program that drops its environment: that program can
/// already type into any terminal over this socket.
fn allowed(agents: &AgentRegistry, caller: &Caller) -> Result<(), String> {
    match (caller.terminal, caller.token.as_deref()) {
        (None, None) => Ok(()),
        (Some(terminal), Some(token)) => {
            if !crate::tasks::admitted(agents, terminal, token) {
                return Err(format!(
                    "the schedule request for terminal {terminal} was refused"
                ));
            }
            if caller.space.as_deref() == Some(QASS_SPACE) {
                Ok(())
            } else {
                Err(
                    "schedules are set by the owner or from qass's own space, not from a \
                     worker's terminal"
                        .into(),
                )
            }
        }
        _ => Err("a schedule request names its terminal and its token together".into()),
    }
}

/// A word an agent or a name may be made of.
fn plain(word: &str, most: usize) -> bool {
    !word.is_empty()
        && word.chars().count() <= most
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// A schedule to add, checked: the row, or what is wrong with it. The cron or the moment is
/// the timing's own check, and the folder is looked at on the probe pool, since it is a path
/// somebody typed.
#[allow(clippy::too_many_arguments)]
async fn made(
    name: String,
    cron: Option<String>,
    at: Option<i64>,
    watch: Option<String>,
    agent: Option<String>,
    cwd: String,
    space: Option<String>,
    prompt: String,
    created_at: i64,
) -> Result<ScheduleRow, String> {
    match (&cron, at, &watch) {
        (Some(text), None, None) => {
            Cron::parse(text).map_err(|e| format!("{text:?}: {e}"))?;
        }
        // A client's clock is a little behind this one's: a few seconds are forgiven.
        (None, Some(at), None) if at + 30 < created_at => {
            return Err("that moment has already passed".into());
        }
        (None, Some(_), None) => {}
        // Run by `sh -c` in the folder, as the owner: the same text they could type there.
        (None, None, Some(command)) => {
            if command.trim().is_empty()
                || command.len() > WATCH_MOST
                || command.chars().any(char::is_control)
            {
                return Err(format!(
                    "a watch is a command of one to {WATCH_MOST} bytes on one line"
                ));
            }
        }
        _ => {
            return Err(
                "give the timing as a cron line, one moment or a command to watch, one of them"
                    .into(),
            );
        }
    }
    // The default agent is the one the person set for qass; a client that does not say is
    // started as `claude`, which the CLI replaces with that setting before it sends.
    let agent = agent.unwrap_or_else(|| "claude".into());
    if !plain(&agent, AGENT_MOST) {
        return Err(format!("{agent:?} is not an agent name"));
    }
    let space = space
        .map(|s| qoop_store::task::one_line(&s))
        .filter(|s| !s.is_empty());
    if space
        .as_ref()
        .is_some_and(|s| s.chars().count() > SPACE_MOST)
    {
        return Err(format!("a space name is at most {SPACE_MOST} characters"));
    }
    // The prompt is typed into a shell's line editor, where a tab completes, ctrl-c or ctrl-u
    // throw the line away and escape changes mode: only a newline, which ends nothing inside
    // the quotes, passes.
    if prompt.trim().is_empty()
        || prompt.len() > SCHEDULE_PROMPT_MAX
        || prompt.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(format!(
            "the prompt is one to {SCHEDULE_PROMPT_MAX} bytes of text, with no tabs or control keys"
        ));
    }
    // A phone or a person types `~/repo`; it is this Mac's home, here, for every client.
    let cwd = match (cwd.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.to_string_lossy())
        }
        _ => cwd,
    };
    if !cwd.starts_with('/') {
        return Err(format!("{cwd} is not an absolute folder"));
    }
    let key = cwd.clone();
    let probe = cwd.clone();
    match crate::blocking::probe(&key, PROBE_DEADLINE, move || {
        std::path::Path::new(&probe).is_dir()
    })
    .await
    {
        Ok(true) => {}
        Ok(false) => return Err(format!("no folder {cwd}")),
        Err(_) => return Err(format!("{cwd} did not answer in time")),
    }
    Ok(ScheduleRow {
        id: 0,
        name,
        cron,
        at,
        agent,
        cwd,
        space,
        prompt,
        watch,
        paused: false,
        created_at,
        last_slot: None,
        last_at: None,
        last_result: None,
        last_terminal: None,
    })
}

/// What a report may say: how the run went, never `claimed`, which is the daemon's word.
/// `checked` is a watch's check that found nothing new.
fn reportable(result: &str) -> bool {
    matches!(result, "started" | "checked")
        || result.starts_with("failed: ")
        || result.starts_with("skipped: ")
}

pub(crate) async fn write(
    id: String,
    caller: Caller,
    action: Box<ScheduleOp>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    books: &Bookkeeping,
) -> ControlResponse {
    if let Err(reason) = allowed(agents, &caller) {
        return failed(id, reason);
    }
    let (store, action) = (&books.store, *action);
    let schedule = match action {
        ScheduleOp::Add {
            name,
            cron,
            at,
            watch,
            agent,
            cwd,
            space,
            prompt,
        } => {
            let row = match made(name, cron, at, watch, agent, cwd, space, prompt, now()).await {
                Ok(row) => row,
                Err(reason) => return failed(id, reason),
            };
            let added = store
                .run(move |s| {
                    if s.schedules()?.len() >= SCHEDULES_MOST {
                        return Ok(None);
                    }
                    s.add_schedule(&row).map(Some)
                })
                .await;
            match added {
                Ok(Some(schedule)) => schedule,
                Ok(None) => {
                    return failed(id, format!("there are {SCHEDULES_MOST} schedules already"));
                }
                Err(e) => return failed(id, why(e)),
            }
        }
        ScheduleOp::Pause { schedule }
        | ScheduleOp::Resume { schedule }
        | ScheduleOp::Remove { schedule }
        | ScheduleOp::Run { schedule }
        | ScheduleOp::Report { schedule, .. } => {
            let row = match store.run(move |s| s.schedule(schedule)).await {
                Ok(Some(row)) => row,
                Ok(None) => return failed(id, format!("no schedule {schedule}")),
                Err(e) => return failed(id, why(e)),
            };
            let applied = match action {
                ScheduleOp::Pause { .. } => store.run(move |s| s.pause_schedule(schedule)).await,
                ScheduleOp::Resume { .. } => {
                    let at = now();
                    store.run(move |s| s.resume_schedule(schedule, at)).await
                }
                ScheduleOp::Remove { .. } => store.run(move |s| s.remove_schedule(schedule)).await,
                ScheduleOp::Run { .. } => {
                    let starting = Starting::new(&books.schedules);
                    if closed(books) {
                        return failed(id, "qoop is restarting; run it again in a moment");
                    }
                    // Its own run: the slot the row holds is the key the run reports under,
                    // and nothing is claimed or moved.
                    let slot = row.last_slot.unwrap_or(0);
                    spawn_run(books, connections, row, slot, starting, true);
                    Ok(true)
                }
                ScheduleOp::Report {
                    slot,
                    result,
                    terminal,
                    seen,
                    ..
                } => {
                    let result = qoop_store::task::one_line(&result);
                    if !reportable(&result) || result.chars().count() > 300 {
                        return failed(id, "a run reports started, skipped: why or failed: why");
                    }
                    if seen.as_ref().is_some_and(|s| s.len() > SEEN_MOST) {
                        return failed(
                            id,
                            format!("a watch's lines are at most {SEEN_MOST} bytes"),
                        );
                    }
                    store
                        .run(move |s| {
                            s.report_schedule(schedule, slot, &result, terminal, seen.as_deref())
                        })
                        .await
                }
                ScheduleOp::Add { .. } => unreachable!("an add is handled above"),
            };
            if let Err(e) = applied {
                return failed(id, why(e));
            }
            schedule
        }
    };
    recount(books, connections).await;
    connections.schedules_changed();
    ControlResponse::ScheduleWritten { id, schedule }
}

/// Count the schedules, fail what a dead daemon left claimed, look once so a client that
/// connects knows the catch-up was decided, and keep looking on a task of its own.
///
/// The first look runs here, before the accept loop: a slot missed while qoop was down is
/// claimed, or skipped, before any client can connect and ask what became of it.
pub(crate) async fn start(books: Bookkeeping, connections: Arc<ConnectionRegistry>) {
    // Before anything looks: what was claimed before this daemon started is a run whose child
    // never reported.
    match books.store.run(|s| s.fail_stale_claims()).await {
        Ok(0) => {}
        Ok(lost) => crate::say!("qoopd: {lost} scheduled runs were lost at the restart"),
        Err(e) => crate::say!("qoopd: schedules: {}", why(e)),
    }
    // Subscribed before the look, so a write that lands during it is not missed.
    let mut news = connections.subscribe_schedules();
    let mut wait = look(&books, &connections).await;
    tokio::spawn(async move {
        loop {
            // Whatever piled up while looking is one reason to look again.
            while matches!(
                news.try_recv(),
                Ok(()) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
            ) {}
            match wait {
                Some(sleep) => {
                    tokio::select! {
                        _ = tokio::time::sleep(sleep) => {}
                        heard = news.recv() => {
                            if matches!(heard, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                                return;
                            }
                        }
                    }
                }
                None => {
                    if matches!(
                        news.recv().await,
                        Err(tokio::sync::broadcast::error::RecvError::Closed)
                    ) {
                        return;
                    }
                }
            }
            wait = look(&books, &connections).await;
        }
    });
}

/// One look at every schedule: claim what is due and start its run. Answers how long to sleep
/// before the next look, or none to park until a write.
async fn look(books: &Bookkeeping, connections: &Arc<ConnectionRegistry>) -> Option<Duration> {
    let (at, zone) = (now(), system_zone());
    let rows = match books.store.run(|s| s.schedules()).await {
        Ok(rows) => rows,
        Err(e) => {
            crate::say!("qoopd: schedules: {}", why(e));
            return Some(RETRY);
        }
    };
    let mut sleep: Option<i64> = None;
    let mut active = 0;
    let mut waiting_on_gate = false;
    let mut claimed = false;
    let mut lost = false;
    for row in rows {
        let decision = decide(&row, at, zone);
        if decision != Decision::Idle {
            active += 1;
        }
        match decision {
            Decision::Idle => {}
            Decision::Wait(due) => {
                let span = (due - at).clamp(1, LOOK_EVERY.as_secs() as i64);
                sleep = Some(sleep.map_or(span, |s| s.min(span)));
            }
            Decision::Fire(slot) => {
                let starting = Starting::new(&books.schedules);
                if closed(books) {
                    waiting_on_gate = true;
                    continue;
                }
                // A watch's check keeps the last run shown: most checks start nothing.
                let result = row.watch.is_none().then(|| "claimed".to_string());
                if claim(books, &row, slot, at, result).await {
                    claimed = true;
                    spawn_run(books, connections, row, slot, starting, false);
                } else {
                    lost = true;
                }
            }
            Decision::Skip(slot, because) => {
                if closed(books) {
                    waiting_on_gate = true;
                    continue;
                }
                crate::say!("qoopd: schedule {} skipped: {because}", row.name);
                if claim(books, &row, slot, at, Some(format!("skipped: {because}"))).await {
                    claimed = true;
                } else {
                    lost = true;
                }
            }
        }
    }
    set_active(books, connections, active);
    if claimed {
        connections.schedules_changed();
        return Some(Duration::ZERO);
    }
    // Going: nothing is claimed any more, and nothing wakes a daemon that is not coming back.
    if books.stopping.load(Ordering::SeqCst) {
        return None;
    }
    // A slot another look took first, or a store that did not answer: looked at again soon, so
    // that a slot that is still due is not left until the next write.
    if waiting_on_gate || lost {
        return Some(RETRY);
    }
    sleep.map(|s| Duration::from_secs(s as u64))
}

/// Take `slot` of `row` for a run. False when somebody else did (a second look, a pause, a
/// remove), or the store did not answer.
async fn claim(
    books: &Bookkeeping,
    row: &ScheduleRow,
    slot: i64,
    at: i64,
    result: Option<String>,
) -> bool {
    let (id, expected) = (row.id, row.last_slot);
    match books
        .store
        .run(move |s| s.claim_schedule(id, expected, slot, at, result.as_deref()))
        .await
    {
        Ok(won) => won,
        Err(e) => {
            crate::say!("qoopd: schedules: {}", why(e));
            false
        }
    }
}

/// The binary this daemon runs as, which is the one a handover moved onto.
fn own_exe(books: &Bookkeeping) -> Option<PathBuf> {
    books.restart.exe().or_else(|| std::env::current_exe().ok())
}

/// Start `qoop schedule fire` for the run of `slot` on a task of its own, never awaited by the
/// loop: opening a terminal takes seconds and the next schedule's time does not wait for it.
/// A run that ends without having reported is written down as failed. `manual` is the owner
/// asking for it now, which a run still going does not hold back.
fn spawn_run(
    books: &Bookkeeping,
    connections: &Arc<ConnectionRegistry>,
    row: ScheduleRow,
    slot: i64,
    starting: Starting,
    manual: bool,
) {
    let (books, connections) = (books.clone(), connections.clone());
    tokio::spawn(async move {
        let (id, name) = (row.id, row.name.clone());
        let (watch, terminal) = (row.watch.is_some(), row.last_terminal);
        // A watch's run is handed the lines its last check saw: none is a watch never checked.
        let seen = if watch {
            match books.store.run(move |s| s.watch_seen(id)).await {
                Ok(seen) => seen,
                Err(e) => {
                    crate::say!("qoopd: schedule {name}: {}", why(e));
                    drop(starting);
                    connections.wake_leaving();
                    return;
                }
            }
        } else {
            None
        };
        let ran = match own_exe(&books) {
            Some(exe) => run_child(exe, row.cwd, id, slot, manual, seen).await,
            None => Err("this daemon does not know its own binary".to_string()),
        };
        if let Err(reason) = ran {
            crate::say!("qoopd: schedule {name}: {reason}");
            // A run the owner asked for, or a watch's check, claimed nothing that says
            // `claimed`, so it is written down as it ended; a watch keeps its last run's tab.
            let failed = books
                .store
                .run(move |s| {
                    if manual || watch {
                        let terminal = terminal.filter(|_| watch);
                        s.report_schedule(id, slot, &format!("failed: {reason}"), terminal, None)
                    } else {
                        s.fail_claimed_schedule(id, slot, &reason)
                    }
                })
                .await;
            if matches!(failed, Ok(true)) {
                connections.schedules_changed();
            }
        }
        // A daemon that was kept from leaving by this run may go now.
        drop(starting);
        connections.wake_leaving();
    });
}

/// Run the child in the schedule's folder (the daemon's own may be a removed worktree) with
/// nothing of a terminal's identity, so what it does is the owner's and not a worker's, and
/// wait for it, polling, so that no thread is held while it opens a terminal. A child that
/// takes longer than [`RUN_PATIENCE`] is killed. A watch's child gets its last check's lines
/// on its stdin (`--seen`).
async fn run_child(
    exe: PathBuf,
    cwd: String,
    id: i64,
    slot: i64,
    manual: bool,
    seen: Option<String>,
) -> Result<(), String> {
    let piped = seen.is_some();
    let mut child = crate::blocking::run_on(crate::blocking::Pool::Lifecycle, move || {
        let mut command = std::process::Command::new(&exe);
        command
            .args([
                "schedule",
                "fire",
                &id.to_string(),
                "--slot",
                &slot.to_string(),
            ])
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if manual {
            command.arg("--now");
        }
        if piped {
            command.arg("--seen").stdin(std::process::Stdio::piped());
        }
        for name in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
            command.env_remove(name);
        }
        let child = command.spawn().map_err(|e| {
            if std::path::Path::new(&cwd).is_dir() {
                format!("could not start the run: {e}")
            } else {
                format!("the folder {cwd} is gone")
            }
        })?;
        Ok::<_, String>(child)
    })
    .await
    .map_err(|e| format!("could not start the run: {e}"))??;
    // The child reads its stdin before anything else. Written on a thread of its own, not the
    // pool's: a child stalled before reading is then only this run's, killed at the deadline
    // below, which breaks the pipe and ends the write.
    if let (Some(seen), Some(mut stdin)) = (seen, child.stdin.take()) {
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(seen.as_bytes());
        });
    }
    let deadline = tokio::time::Instant::now() + RUN_PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("the run ended with {status}")),
            Ok(None) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the run did not finish in time".into());
            }
            Err(e) => return Err(format!("the run could not be waited for: {e}")),
        }
    }
}
