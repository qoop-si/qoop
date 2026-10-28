//! Usage counts (ADR-023): how many agents, terminals, spaces and questions a machine had in a
//! day, sent once the day is over. Numbers and names from the fixed lists below only: never a
//! path, a name, a command or anything an agent printed.
//!
//! Nothing is counted until the notice line was shown in a real terminal (`telemetry-noticed`),
//! and nothing while `config.toml` says anything but `telemetry = true`. Turning it off deletes
//! every count, the id and the notice mark at once, and stops a send in flight. The daemon runs
//! one task: every five minutes it looks at those conditions, samples what is open, and when
//! its random moment has come sends the finished days with curl, the way the update check
//! fetches releases.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::config::TelemetrySetting;
use crate::store::{StoreHandle, now};

/// Where the finished days go.
pub const URL: &str = "https://qoop.si/api/telemetry";

/// The line shown once before anything is counted.
pub const NOTICE: &str = "qoop sends daily usage counts (agents, terminals, questions, versions; \
                          never content). Stop: qoop telemetry off";

/// How long the notice stays on screen before it counts as seen.
pub const NOTICE_HOLD: Duration = Duration::from_secs(5);

/// How often the task looks, samples and maybe sends.
const EVERY: Duration = Duration::from_secs(5 * 60);
/// Each send happens at a random moment within this many seconds of the last.
const WINDOW: i64 = 6 * 60 * 60;
/// A day the hub refuses (400) this many times is dropped: it would never be taken.
const REFUSED_MOST: u32 = 3;

// The keys, all of them: nothing outside this list is ever written.
const SPACES_MAX: &str = "spaces_max";
const SPACES_CREATED: &str = "spaces_created";
const TERMS_OPEN_MAX: &str = "terms_open_max";
const TERMS_OPEN_SUM: &str = "terms_open_sum";
const TERMS_PER_SPACE_MAX: &str = "terms_per_space_max";
const TERMS_OPENED: &str = "terms_opened";
const AGENTS_MAX: &str = "agents_max";
const AGENTS_SUM: &str = "agents_sum";
const AGENTS_WORKING_MAX: &str = "agents_working_max";
const SAMPLES: &str = "samples";
const PHONE: &str = "phone";
const Q_ANSWERED: &str = "q_answered";
const Q_DISMISSED: &str = "q_dismissed";
const Q_CLOSED_QASS: &str = "q_closed_qass";
const Q_WAIT_SUM: &str = "q_wait_sum_s";
const Q_WAIT_N: &str = "q_wait_n";
const SCHEDULE_RUNS: &str = "schedule_runs";

/// A count of something that happened, at the place it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    SpaceCreated,
    TerminalOpened,
    /// A card the daemon wrote for an agent waiting on the owner.
    DraftAsked,
    /// A card for a question dialog on an agent's screen.
    DialogAsked,
    /// A card qass wrote.
    QassAsked,
    /// The owner's answer reached the agent, after `waited` seconds.
    Answered {
        waited: i64,
    },
    Dismissed,
    ClosedByQass,
    ScheduleRan,
}

/// Agent kinds counted by name; every other agent is `other`.
const KINDS: &[(&str, &str)] = &[
    ("claude", "agents_by_kind_max.claude"),
    ("codex", "agents_by_kind_max.codex"),
    ("opencode", "agents_by_kind_max.opencode"),
    ("gemini", "agents_by_kind_max.gemini"),
    ("cursor", "agents_by_kind_max.cursor"),
    ("aider", "agents_by_kind_max.aider"),
    ("amp", "agents_by_kind_max.amp"),
    ("goose", "agents_by_kind_max.goose"),
    ("droid", "agents_by_kind_max.droid"),
    ("copilot", "agents_by_kind_max.copilot"),
    ("qwen", "agents_by_kind_max.qwen"),
    ("other", "agents_by_kind_max.other"),
];

/// Terminal apps, as the interface names them in its hello.
pub const TERMS: &[(&str, &str)] = &[
    ("ghostty", "term.ghostty"),
    ("iterm", "term.iterm"),
    ("apple_terminal", "term.apple_terminal"),
    ("kitty", "term.kitty"),
    ("wezterm", "term.wezterm"),
    ("alacritty", "term.alacritty"),
    ("warp", "term.warp"),
    ("vscode", "term.vscode"),
    ("windows_terminal", "term.windows_terminal"),
    ("konsole", "term.konsole"),
    ("gnome", "term.gnome"),
    ("other", "term.other"),
];

/// Multiplexers the interface can run inside.
pub const MUXES: &[(&str, &str)] = &[
    ("tmux", "mux.tmux"),
    ("zellij", "mux.zellij"),
    ("screen", "mux.screen"),
];

const ASKED: &[(&str, &str)] = &[
    ("draft", "q_created.draft"),
    ("dialog", "q_created.dialog"),
    ("qass", "q_created.qass"),
];

fn key_of(list: &[(&str, &'static str)], name: &str) -> Option<&'static str> {
    list.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}

/// The key for an agent program: its own when it has one, else `other`.
fn kind_key(program: &str) -> &'static str {
    key_of(KINDS, program).unwrap_or("agents_by_kind_max.other")
}

/// The fixed name for a terminal app a client sent, `other` for anything unknown. A received
/// string is only ever compared, never stored.
pub fn term_name(sent: &str) -> &'static str {
    TERMS
        .iter()
        .find(|(n, _)| *n == sent)
        .map_or("other", |(n, _)| *n)
}

/// The fixed name for a multiplexer a client sent, `None` for anything unknown.
pub fn mux_name(sent: &str) -> Option<&'static str> {
    MUXES.iter().find(|(n, _)| *n == sent).map(|(n, _)| *n)
}

/// What is open right now, read by the daemon for each sample.
#[derive(Default, Debug)]
pub struct Gauges {
    pub terminals: u32,
    pub most_in_one_space: u32,
    /// The program of each detected agent, shells and ordinary commands left out.
    pub agents: Vec<String>,
    pub working: u32,
    /// The fixed names of the terminal apps and multiplexers of every interface attached now.
    pub terms: Vec<&'static str>,
    pub muxes: Vec<&'static str>,
    pub phone: bool,
}

/// The UTC day of a unix time, `YYYY-MM-DD`.
pub fn day_of(unix: i64) -> String {
    let n = crate::schedule::Naive::from_secs(unix);
    format!("{:04}-{:02}-{:02}", n.year, n.month, n.day)
}

/// Where the mark that the notice was seen lives: this machine's own state directory, never the
/// config directory, which people sync between machines (a synced mark would let a host that
/// never showed the line count).
pub fn noticed_path() -> PathBuf {
    crate::paths::runtime_dir()
        .map(|dir| crate::paths::state_location(&dir))
        .unwrap_or_else(|_| std::env::temp_dir().join("qoop-no-state"))
        .join("telemetry-noticed")
}

/// Whether the environment asks for no counting: `DO_NOT_TRACK` set to anything but `0`, or
/// `QOOP_TELEMETRY` set to `0`, `false` or `off`.
pub fn env_opted_out() -> bool {
    opted_out(
        std::env::var("DO_NOT_TRACK").ok().as_deref(),
        std::env::var("QOOP_TELEMETRY").ok().as_deref(),
    )
}

fn opted_out(do_not_track: Option<&str>, qoop: Option<&str>) -> bool {
    do_not_track.is_some_and(|v| !v.is_empty() && v != "0")
        || qoop.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "off"))
}

/// Whether this process may mark the notice as seen: a person at a real terminal, not an agent
/// in a qoop terminal and not a script, and no opt out in the environment.
pub fn may_notice() -> bool {
    // SAFETY: isatty only reads the descriptor's state.
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    tty && !in_qoop_terminal() && !env_opted_out()
}

/// Whether this process runs in a qoop terminal, where an agent may be the one typing: either
/// variable counts, so dropping one does not pass for the owner.
pub fn in_qoop_terminal() -> bool {
    let var = |k: &str| crate::legacy::var(k).ok().filter(|v| !v.is_empty());
    var("QOOP_TERMINAL").is_some() || var("QOOP_TOKEN").is_some()
}

/// Mark the notice as seen today, after it has been on screen for `NOTICE_HOLD` and only when
/// [`may_notice`] holds. Answers whether it was marked.
pub fn mark_noticed() -> bool {
    if !may_notice() {
        return false;
    }
    let path = noticed_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, format!("{}\n", day_of(now()))).is_ok()
}

/// What the environment opt out does in a process that is not the daemon: `telemetry = false`
/// in the file, and when the file cannot be written the notice mark goes instead, so nothing is
/// counted until the line is shown again (which `may_notice` refuses while the variable is
/// set). Answers a warning to print, if any. qoop always goes on.
pub fn opt_out_from_env() -> Option<String> {
    let path = crate::config::path();
    if matches!(
        crate::config::telemetry_from(&path),
        TelemetrySetting::Off(_)
    ) {
        return None;
    }
    match crate::config::set_telemetry(&path, false) {
        Ok(()) => None,
        Err(error) => {
            let _ = std::fs::remove_file(noticed_path());
            Some(format!(
                "qoop: usage counts stay off for DO_NOT_TRACK, but {error}"
            ))
        }
    }
}

/// The send target. A debug build may be pointed elsewhere with `QOOP_TELEMETRY_URL`, for a
/// local hub; a release build never reads it.
pub fn debug_url() -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var("QOOP_TELEMETRY_URL")
            .ok()
            .filter(|u| !u.is_empty())
    } else {
        None
    }
}

/// Where the counting stands, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    On,
    /// Nothing counted or sent for now; nothing deleted.
    Paused(String),
    /// Counts deleted, nothing counted.
    Off(String),
}

/// What the daemon's count calls go to, once its task runs.
static ACTIVE: OnceLock<Arc<Telemetry>> = OnceLock::new();

/// Count `event` today, when counting is on. One line at the place it happened; a no-op in a
/// daemon whose task does not run (a test, a build that is not a release).
pub fn count(event: Event) {
    if let Some(t) = ACTIVE.get() {
        let _ = t.count(event);
    }
}

/// A client attached: its terminal app and multiplexer, or that it is the phone, marked for
/// today at once, so one that leaves before the next sample still counts.
pub fn attach(term: Option<&'static str>, mux: Option<&'static str>, phone: bool) {
    if let Some(t) = ACTIVE.get() {
        t.attached(term, mux, phone);
    }
}

/// The answer to a `Telemetry` control request. A daemon that keeps no counts answers that it
/// counts nothing (`counting: false`); the caller then turns the file off and clears the store
/// itself.
pub async fn answer(
    id: String,
    op: crate::control::TelemetryOp,
) -> crate::control::ControlResponse {
    use crate::control::{ControlResponse, TelemetryOp};
    let Some(t) = ACTIVE.get() else {
        return ControlResponse::Telemetry {
            id,
            state: "off".into(),
            why: Some("this qoop is not a release build and keeps no usage counts".into()),
            counting: false,
            days: None,
        };
    };
    let mut unwritten = None;
    let standing = match op {
        TelemetryOp::Off => {
            unwritten = t.turn_off().await;
            t.standing()
        }
        TelemetryOp::On => t.turn_on().await,
        TelemetryOp::Status | TelemetryOp::Show => t.check().await,
    };
    let days = match op {
        TelemetryOp::Show => Some(t.show().await),
        _ => None,
    };
    let (state, why) = match standing {
        Standing::On => ("on", None),
        Standing::Paused(why) => ("paused", Some(why)),
        Standing::Off(why) => match unwritten {
            Some(error) => (
                "off",
                Some(format!("{why}; config.toml was not changed: {error}")),
            ),
            None => ("off", Some(why)),
        },
    };
    ControlResponse::Telemetry {
        id,
        state: state.into(),
        why,
        counting: true,
        days,
    }
}

pub struct Telemetry {
    store: StoreHandle,
    /// Read inside every store write, so a write queued before an off cannot land after its
    /// purge: the store runs them in order and the off flips this first.
    counting: Arc<AtomicBool>,
    gate: Mutex<Gate>,
    /// Held by whatever turns counting on or deletes it all, so two of them never interleave.
    /// Taken before `sending` when both are needed.
    turning: tokio::sync::Mutex<()>,
    /// Held for one day's send, from its payload to its answer: an off waits on it, so it
    /// deletes nothing a send is still writing and acknowledges only once curl is gone.
    sending: tokio::sync::Mutex<()>,
    config: PathBuf,
    noticed: PathBuf,
    url: String,
}

#[derive(Default)]
struct Gate {
    /// Turned off through the socket; holds for this daemon's life even if the file could not
    /// be written.
    off: bool,
    /// Stops the send in flight.
    cancel: Option<Arc<Mutex<bool>>>,
}

impl Telemetry {
    pub fn new(store: StoreHandle, url: String) -> Arc<Self> {
        Self::with_files(store, url, crate::config::path(), noticed_path())
    }

    /// The same with its file and notice mark named, as a test gives them.
    pub fn with_files(
        store: StoreHandle,
        url: String,
        config: PathBuf,
        noticed: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Telemetry {
            store,
            counting: Arc::new(AtomicBool::new(false)),
            gate: Mutex::new(Gate::default()),
            turning: tokio::sync::Mutex::new(()),
            sending: tokio::sync::Mutex::new(()),
            config,
            noticed,
            url,
        })
    }

    /// Become what [`count`] reaches. Once per process.
    pub fn activate(self: &Arc<Self>) {
        let _ = ACTIVE.set(self.clone());
    }

    fn gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The conditions, read now: the socket's off, the file, the notice mark.
    pub fn standing(&self) -> Standing {
        if self.gate().off {
            return Standing::Off("turned off (qoop telemetry off)".into());
        }
        match crate::config::telemetry_from(&self.config) {
            TelemetrySetting::Unreadable(why) => return Standing::Paused(why),
            TelemetrySetting::Off(at) => {
                return Standing::Off(format!("{at} in {}", self.config.display()));
            }
            TelemetrySetting::On => {}
        }
        if !self.noticed.exists() {
            return Standing::Off("the notice line has not been shown yet".into());
        }
        Standing::On
    }

    /// Look at the conditions and act on them: count, pause, or delete everything.
    pub async fn check(&self) -> Standing {
        let _turning = self.turning.lock().await;
        let standing = self.standing();
        match &standing {
            // Only while no off is set: one that came in after `standing` was read wins.
            Standing::On => {
                let gate = self.gate();
                self.counting.store(!gate.off, Ordering::SeqCst);
            }
            Standing::Paused(_) => self.counting.store(false, Ordering::SeqCst),
            Standing::Off(_) => self.purge().await,
        }
        standing
    }

    /// Off now: no more counting, the send in flight stopped and waited for, every count, the
    /// id and the notice mark deleted, and `telemetry = false` written. Answers why the file
    /// could not be written, if it could not; the off holds for this daemon's life anyway.
    pub async fn turn_off(&self) -> Option<String> {
        self.gate().off = true;
        let _turning = self.turning.lock().await;
        self.purge().await;
        // Not rewritten when it already says off: every opted-out command lands here, and a
        // rewrite could race another process saving the file.
        if matches!(
            crate::config::telemetry_from(&self.config),
            TelemetrySetting::Off(_)
        ) {
            return None;
        }
        crate::config::set_telemetry(&self.config, false).err()
    }

    /// Lift the socket's off. Counting starts again once the file says on and the notice was
    /// shown again.
    pub async fn turn_on(&self) -> Standing {
        self.gate().off = false;
        self.check().await
    }

    /// Stop counting and any send, wait for the send to be gone, then delete it all. Called
    /// with `turning` held.
    async fn purge(&self) {
        {
            let gate = self.gate();
            self.counting.store(false, Ordering::SeqCst);
            if let Some(cancel) = &gate.cancel {
                *cancel
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            }
        }
        let _sending = self.sending.lock().await;
        let _ = self.store.run(|s| s.telemetry_purge()).await;
        let _ = std::fs::remove_file(&self.noticed);
    }

    fn count(&self, event: Event) -> Option<tokio::task::JoinHandle<()>> {
        let one = |key: &'static str| vec![(key, 1.0)];
        let adds = match event {
            Event::SpaceCreated => one(SPACES_CREATED),
            Event::TerminalOpened => one(TERMS_OPENED),
            Event::DraftAsked => one(ASKED[0].1),
            Event::DialogAsked => one(ASKED[1].1),
            Event::QassAsked => one(ASKED[2].1),
            Event::Answered { waited } => vec![
                (Q_ANSWERED, 1.0),
                (Q_WAIT_SUM, waited.max(0) as f64),
                (Q_WAIT_N, 1.0),
            ],
            Event::Dismissed => one(Q_DISMISSED),
            Event::ClosedByQass => one(Q_CLOSED_QASS),
            Event::ScheduleRan => one(SCHEDULE_RUNS),
        };
        self.write(adds, Vec::new())
    }

    /// One sample of what is open.
    pub fn sample(&self, gauges: &Gauges, spaces: u32) -> Option<tokio::task::JoinHandle<()>> {
        let mut kinds: Vec<(&'static str, f64)> = Vec::new();
        for program in &gauges.agents {
            let key = kind_key(program);
            match kinds.iter_mut().find(|(k, _)| *k == key) {
                Some((_, n)) => *n += 1.0,
                None => kinds.push((key, 1.0)),
            }
        }
        let mut peaks = vec![
            (SPACES_MAX, f64::from(spaces)),
            (TERMS_OPEN_MAX, f64::from(gauges.terminals)),
            (TERMS_PER_SPACE_MAX, f64::from(gauges.most_in_one_space)),
            (AGENTS_MAX, gauges.agents.len() as f64),
            (AGENTS_WORKING_MAX, f64::from(gauges.working)),
        ];
        peaks.extend(kinds);
        for name in &gauges.terms {
            peaks.extend(key_of(TERMS, name).map(|k| (k, 1.0)));
        }
        for name in &gauges.muxes {
            peaks.extend(key_of(MUXES, name).map(|k| (k, 1.0)));
        }
        if gauges.phone {
            peaks.push((PHONE, 1.0));
        }
        let adds = vec![
            (TERMS_OPEN_SUM, f64::from(gauges.terminals)),
            (AGENTS_SUM, gauges.agents.len() as f64),
            (SAMPLES, 1.0),
        ];
        self.write(adds, peaks)
    }

    fn attached(&self, term: Option<&'static str>, mux: Option<&'static str>, phone: bool) {
        let peaks = term
            .and_then(|t| key_of(TERMS, t))
            .into_iter()
            .chain(mux.and_then(|m| key_of(MUXES, m)))
            .chain(phone.then_some(PHONE))
            .map(|k| (k, 1.0))
            .collect();
        let _ = self.write(Vec::new(), peaks);
    }

    /// Add `adds` and raise `peaks` for today, in one store call that checks the counting flag
    /// itself, with the day's version and platform kept on its first write.
    fn write(
        &self,
        adds: Vec<(&'static str, f64)>,
        peaks: Vec<(&'static str, f64)>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !self.counting.load(Ordering::SeqCst) || (adds.is_empty() && peaks.is_empty()) {
            return None;
        }
        let (store, counting) = (self.store.clone(), self.counting.clone());
        let handle = tokio::runtime::Handle::try_current().ok()?;
        Some(handle.spawn(async move {
            let _ = store
                .run(move |s| {
                    if !counting.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    let day = day_of(now());
                    // The id is made by the first count, never by a read: a `show` or a send
                    // racing an off cannot bring it back.
                    if let Some(fresh) = new_id() {
                        s.telemetry_keep("id", &fresh)?;
                    }
                    s.telemetry_keep(&format!("meta:{day}"), &meta())?;
                    for (key, by) in adds {
                        s.telemetry_bump(&day, key, by)?;
                    }
                    for (key, value) in peaks {
                        s.telemetry_peak(&day, key, value)?;
                    }
                    Ok(())
                })
                .await;
        }))
    }

    /// The install id, as the first count made it; `None` before any count.
    async fn id(&self) -> Option<String> {
        self.store
            .run(|s| s.telemetry_get("id"))
            .await
            .ok()
            .flatten()
    }

    /// The JSON for one day, exactly as it is sent.
    pub async fn payload(&self, day: &str) -> Option<serde_json::Value> {
        let id = self.id().await?;
        let d = day.to_string();
        let (rows, meta) = self
            .store
            .run(move |s| Ok((s.telemetry_day(&d)?, s.telemetry_get(&format!("meta:{d}"))?)))
            .await
            .ok()?;
        Some(build(&id, day, &rows, meta.as_deref()))
    }

    /// Every day not yet sent with its JSON, today's so far last: what `qoop telemetry show`
    /// prints.
    pub async fn show(&self) -> serde_json::Value {
        let days = self
            .store
            .run(|s| s.telemetry_days())
            .await
            .unwrap_or_default();
        let mut out = Vec::new();
        for day in days {
            out.extend(self.payload(&day).await);
        }
        serde_json::Value::Array(out)
    }

    /// The daemon's task: look, sample and send every `EVERY`, for as long as it lives.
    pub async fn run(self: Arc<Self>, gauges: impl Fn() -> Gauges + Send + 'static) {
        loop {
            if self.check().await == Standing::On {
                let spaces = self.store.run(|s| s.workspace_count()).await.unwrap_or(0);
                let _ = self.sample(&gauges(), u32::try_from(spaces).unwrap_or(0));
                self.send_when_due().await;
            }
            tokio::time::sleep(EVERY).await;
        }
    }

    /// Send the finished days if the persisted moment has come; pick the next one either way.
    /// The moment is kept in the store, so a daemon that lives a minute does not push it back.
    async fn send_when_due(&self) {
        let unix = now();
        let next = self
            .store
            .run(|s| s.telemetry_get("next_send"))
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok());
        let due = match next {
            Some(at) => unix >= at,
            None => false,
        };
        if due {
            self.send_finished(&day_of(unix)).await;
        }
        if due || next.is_none() {
            // Under `sending` and only while counting: an off that purged meanwhile is not
            // undone by a fresh deadline.
            let _sending = self.sending.lock().await;
            if self.counting.load(Ordering::SeqCst) {
                let at = unix + random_below(WINDOW);
                let counting = self.counting.clone();
                // Checked again under the store's own lock: a call that outlived its deadline
                // still runs, and must not land after a purge.
                let _ = self
                    .store
                    .run(move |s| {
                        if counting.load(Ordering::SeqCst) {
                            s.telemetry_set("next_send", &at.to_string())?;
                        }
                        Ok(())
                    })
                    .await;
            }
        }
    }

    /// One POST per finished day, oldest first; days older than the hub takes are dropped.
    async fn send_finished(&self, today: &str) {
        let days = self
            .store
            .run(|s| s.telemetry_days())
            .await
            .unwrap_or_default();
        // The hub takes no day older than 31 days: those go without a send.
        let oldest = day_of(now() - 31 * 86_400);
        let (stale, finished): (Vec<String>, Vec<String>) = days
            .into_iter()
            .filter(|d| d.as_str() < today)
            .partition(|d| *d < oldest);
        for old in stale {
            let _ = self.store.run(move |s| s.telemetry_drop_day(&old)).await;
        }
        for day in &finished {
            // Held to the end of this day's send: a purge waits for it.
            let _sending = self.sending.lock().await;
            if !self.counting.load(Ordering::SeqCst) {
                return;
            }
            let Some(body) = self.payload(day).await else {
                return;
            };
            let cancel = Arc::new(Mutex::new(false));
            // Read before the gate is taken: `standing` takes it too. An off in between is
            // caught by `gate.off`, which is set under the gate.
            let standing = self.standing();
            {
                let mut gate = self.gate();
                // `counting` too: a purge clears it under this gate before it waits for the send.
                if gate.off || !self.counting.load(Ordering::SeqCst) || standing != Standing::On {
                    return;
                }
                gate.cancel = Some(cancel.clone());
            }
            let (url, text) = (self.url.clone(), body.to_string());
            let answer = tokio::task::spawn_blocking(move || post(&url, &text, &cancel))
                .await
                .unwrap_or(Answer::Failed);
            let off = {
                let mut gate = self.gate();
                gate.cancel = None;
                gate.off || !self.counting.load(Ordering::SeqCst)
            };
            let day = day.clone();
            match answer {
                // Taken even if counting stopped meanwhile: never sent twice.
                Answer::Status(204) => {
                    let _ = self.store.run(move |s| s.telemetry_drop_day(&day)).await;
                }
                Answer::Status(400) if !off => {
                    let counting = self.counting.clone();
                    let _ = self
                        .store
                        .run(move |s| {
                            // Again under the store's lock: a late call must not write after a
                            // purge.
                            if !counting.load(Ordering::SeqCst) {
                                return Ok(());
                            }
                            let key = format!("refused:{day}");
                            let n = s
                                .telemetry_get(&key)?
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(0)
                                + 1;
                            if n >= REFUSED_MOST {
                                s.telemetry_drop_day(&day)
                            } else {
                                s.telemetry_set(&key, &n.to_string())
                            }
                        })
                        .await;
                }
                // Anything else (a redirect, 429, 5xx, a portal's 200, no network) keeps it.
                _ => {}
            }
            if off {
                return;
            }
        }
    }
}

/// The day's JSON from its rows (see the plan's wire format).
fn build(id: &str, day: &str, rows: &[(String, f64)], meta: Option<&str>) -> serde_json::Value {
    let n = |key: &str| rows.iter().find(|(k, _)| k == key).map_or(0.0, |(_, v)| *v);
    let whole = |key: &str| n(key).round().max(0.0) as u64;
    let mean = |sum: &str| {
        let samples = n(SAMPLES);
        if samples > 0.0 {
            (n(sum) / samples * 10.0).round() / 10.0
        } else {
            0.0
        }
    };
    let map = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(name, key)| ((*name).to_string(), serde_json::json!(whole(key))))
            .collect::<serde_json::Map<_, _>>()
    };
    let flags = |list: &[(&str, &str)]| {
        list.iter()
            .map(|(name, key)| ((*name).to_string(), serde_json::json!(n(key) > 0.0)))
            .collect::<serde_json::Map<_, _>>()
    };
    let meta: serde_json::Value = meta
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or_else(|| serde_json::from_str(&self::meta()).unwrap_or_default());
    serde_json::json!({
        "id": id,
        "day": day,
        "v": meta["v"],
        "os": meta["os"],
        "arch": meta["arch"],
        "phone": n(PHONE) > 0.0,
        "term": flags(TERMS),
        "mux": flags(MUXES),
        "spaces_max": whole(SPACES_MAX),
        "spaces_created": whole(SPACES_CREATED),
        "terms_open_max": whole(TERMS_OPEN_MAX),
        "terms_open_avg": mean(TERMS_OPEN_SUM),
        "terms_per_space_max": whole(TERMS_PER_SPACE_MAX),
        "terms_opened": whole(TERMS_OPENED),
        "agents_max": whole(AGENTS_MAX),
        "agents_avg": mean(AGENTS_SUM),
        "agents_working_max": whole(AGENTS_WORKING_MAX),
        "agents_by_kind_max": map(KINDS),
        "q_created": map(ASKED),
        "q_answered": whole(Q_ANSWERED),
        "q_dismissed": whole(Q_DISMISSED),
        "q_closed_qass": whole(Q_CLOSED_QASS),
        "q_wait_sum_s": whole(Q_WAIT_SUM),
        "q_wait_n": whole(Q_WAIT_N),
        "schedule_runs": whole(SCHEDULE_RUNS),
    })
}

/// This build's version and platform, as a day keeps them.
fn meta() -> String {
    let os = match std::env::consts::OS {
        os @ ("macos" | "linux" | "windows") => os,
        _ => "other",
    };
    let arch = match std::env::consts::ARCH {
        arch @ ("aarch64" | "x86_64") => arch,
        _ => "other",
    };
    serde_json::json!({"v": env!("CARGO_PKG_VERSION"), "os": os, "arch": arch}).to_string()
}

/// A random UUID v4: not derived from anything about the machine or the person.
fn new_id() -> Option<String> {
    let hex = crate::paths::random_hex().ok()?;
    let mut h: Vec<char> = hex.chars().collect();
    h[12] = '4';
    h[16] = ['8', '9', 'a', 'b'][h[16].to_digit(16).unwrap_or(0) as usize % 4];
    let s: String = h.into_iter().collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    ))
}

/// A uniform number in `0..below`, from the system's randomness.
fn random_below(below: i64) -> i64 {
    let n = crate::paths::random_hex()
        .ok()
        .and_then(|h| u64::from_str_radix(&h[..16], 16).ok())
        .unwrap_or(0);
    (n % below.max(1) as u64) as i64
}

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Status(u16),
    Failed,
}

/// One POST with curl. `-q` first, so no `.curlrc` adds a redirect or a header; https only and
/// no redirects. `cancel` kills it. A debug build pointed at a local hub with
/// `QOOP_TELEMETRY_URL` may use http and says where it forwards from, as a proxy in front of
/// the hub would.
fn post(url: &str, body: &str, cancel: &Mutex<bool>) -> Answer {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let local = cfg!(debug_assertions) && url != URL;
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "-sS",
        "--proto",
        if local { "=http,https" } else { "=https" },
        "--max-redirs",
        "0",
        "--max-time",
        "10",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
    ]);
    if local {
        command.args(["-H", "X-Forwarded-For: 127.0.0.1"]);
    }
    command
        .args(["--data-binary", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let cancelled = || {
        cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    // Started and handed its body under the same lock an off takes: an off comes either before
    // (nothing starts) or after (the child is killed below).
    let mut child = {
        let off = cancelled();
        if *off {
            return Answer::Failed;
        }
        let Ok(mut child) = command.spawn() else {
            return Answer::Failed;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.as_bytes());
        }
        child
    };
    loop {
        if *cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Answer::Failed;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut stdout, &mut out);
                }
                return match (status.success(), out.trim().parse::<u16>()) {
                    (true, Ok(code)) => Answer::Status(code),
                    _ => Answer::Failed,
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return Answer::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreHandle {
        StoreHandle::new(qoop_store::Store::in_memory().expect("store"))
    }

    /// A telemetry whose file and notice mark live in `dir`, the notice already seen.
    fn seen(dir: &std::path::Path, url: &str) -> Arc<Telemetry> {
        std::fs::write(dir.join("noticed"), "2026-10-24\n").expect("notice");
        Telemetry::with_files(
            store(),
            url.into(),
            dir.join("config.toml"),
            dir.join("noticed"),
        )
    }

    async fn landed(write: Option<tokio::task::JoinHandle<()>>) {
        if let Some(write) = write {
            write.await.expect("write");
        }
    }

    async fn today(t: &Telemetry) -> Vec<(String, f64)> {
        let day = day_of(now());
        t.store
            .run(move |s| s.telemetry_day(&day))
            .await
            .expect("day")
    }

    #[test]
    fn the_environment_opts_out_with_do_not_track_or_qoop_telemetry_zero() {
        assert!(!opted_out(None, None));
        assert!(opted_out(Some("1"), None));
        assert!(opted_out(Some("yes"), None));
        assert!(!opted_out(Some("0"), None));
        assert!(!opted_out(Some(""), None));
        assert!(opted_out(None, Some("0")));
        assert!(opted_out(None, Some("OFF")));
        assert!(!opted_out(None, Some("1")));
    }

    #[test]
    fn a_received_terminal_name_is_only_ever_one_of_the_fixed_names() {
        assert_eq!(term_name("ghostty"), "ghostty");
        assert_eq!(term_name("../../etc/passwd"), "other");
        assert_eq!(mux_name("tmux"), Some("tmux"));
        assert_eq!(mux_name("byobu"), None);
        assert_eq!(kind_key("claude"), "agents_by_kind_max.claude");
        assert_eq!(kind_key("muse"), "agents_by_kind_max.other");
    }

    #[test]
    fn a_new_id_is_a_version_4_uuid() {
        let id = new_id().expect("id");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(
            parts[3].chars().next(),
            Some('8' | '9' | 'a' | 'b')
        ));
        assert_ne!(new_id(), Some(id));
    }

    #[test]
    fn a_day_is_its_utc_date() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(1_793_491_199), "2026-10-31");
        assert_eq!(day_of(1_793_491_200), "2026-11-01");
    }

    #[test]
    fn the_payload_has_exactly_the_wire_fields() {
        let rows = vec![
            ("agents_max".to_string(), 3.0),
            ("agents_sum".to_string(), 5.0),
            ("samples".to_string(), 2.0),
            ("agents_by_kind_max.claude".to_string(), 2.0),
            ("term.ghostty".to_string(), 1.0),
            ("q_created.dialog".to_string(), 4.0),
            ("phone".to_string(), 1.0),
        ];
        let meta = r#"{"v":"0.1.0","os":"macos","arch":"aarch64"}"#;
        let p = build("id", "2026-10-24", &rows, Some(meta));
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        let mut want = vec![
            "id",
            "day",
            "v",
            "os",
            "arch",
            "phone",
            "term",
            "mux",
            "spaces_max",
            "spaces_created",
            "terms_open_max",
            "terms_open_avg",
            "terms_per_space_max",
            "terms_opened",
            "agents_max",
            "agents_avg",
            "agents_working_max",
            "agents_by_kind_max",
            "q_created",
            "q_answered",
            "q_dismissed",
            "q_closed_qass",
            "q_wait_sum_s",
            "q_wait_n",
            "schedule_runs",
        ];
        want.sort_unstable();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(keys, want);
        assert_eq!(p["v"], "0.1.0");
        assert_eq!(p["agents_max"], 3);
        assert_eq!(p["agents_avg"], 2.5);
        assert_eq!(p["agents_by_kind_max"]["claude"], 2);
        assert_eq!(p["agents_by_kind_max"]["other"], 0);
        assert_eq!(p["term"]["ghostty"], true);
        assert_eq!(p["term"]["iterm"], false);
        assert_eq!(p["q_created"]["dialog"], 4);
        assert_eq!(p["phone"], true);
    }

    #[tokio::test]
    async fn nothing_is_counted_before_the_notice_and_off_in_the_file_deletes_it_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = Telemetry::with_files(
            store(),
            URL.into(),
            dir.path().join("config.toml"),
            dir.path().join("noticed"),
        );
        assert!(matches!(t.check().await, Standing::Off(_)));
        assert!(t.count(Event::TerminalOpened).is_none());
        assert!(today(&t).await.is_empty());

        std::fs::write(dir.path().join("noticed"), "x").expect("notice");
        assert_eq!(t.check().await, Standing::On);
        landed(t.count(Event::TerminalOpened)).await;
        landed(t.count(Event::Answered { waited: 30 })).await;
        assert_eq!(
            today(&t).await,
            vec![
                ("q_answered".into(), 1.0),
                ("q_wait_n".into(), 1.0),
                ("q_wait_sum_s".into(), 30.0),
                ("terms_opened".into(), 1.0),
            ]
        );

        std::fs::write(
            dir.path().join("config.toml"),
            "[daemon]\ntelemetry = false\n",
        )
        .expect("config");
        assert!(matches!(t.check().await, Standing::Off(at) if at.contains("daemon.telemetry")));
        assert!(today(&t).await.is_empty());
        assert!(!dir.path().join("noticed").exists(), "the notice goes too");
        std::fs::write(dir.path().join("config.toml"), "telemetry = true\n").expect("config");
        assert!(
            matches!(t.check().await, Standing::Off(_)),
            "back on in the file still needs the notice again"
        );
    }

    #[tokio::test]
    async fn a_broken_file_pauses_without_deleting_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = seen(dir.path(), URL);
        assert_eq!(t.check().await, Standing::On);
        landed(t.count(Event::ScheduleRan)).await;
        std::fs::write(dir.path().join("config.toml"), "[sleep\n").expect("config");
        assert!(matches!(t.check().await, Standing::Paused(_)));
        assert!(t.count(Event::ScheduleRan).is_none());
        assert_eq!(today(&t).await, vec![("schedule_runs".into(), 1.0)]);
        assert!(dir.path().join("noticed").exists());
    }

    #[tokio::test]
    async fn turning_it_off_stops_counting_at_once_and_deletes_the_id_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = seen(dir.path(), URL);
        assert_eq!(t.check().await, Standing::On);
        landed(t.count(Event::Dismissed)).await;
        let id = t.id().await.expect("id");
        t.turn_off().await;
        assert!(t.count(Event::Dismissed).is_none());
        assert!(today(&t).await.is_empty());
        assert!(!dir.path().join("noticed").exists());
        assert!(matches!(t.standing(), Standing::Off(_)));
        assert!(!id.is_empty());
        assert_eq!(t.id().await, None, "the id goes with an off");
    }

    #[tokio::test]
    async fn a_sample_keeps_the_most_and_the_mean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = seen(dir.path(), URL);
        t.check().await;
        let g = |n: u32| Gauges {
            terminals: n,
            most_in_one_space: n,
            agents: vec!["claude".into(); n as usize],
            working: 1,
            terms: vec!["ghostty"],
            muxes: vec![],
            phone: false,
        };
        landed(t.sample(&g(4), 2)).await;
        landed(t.sample(&g(2), 3)).await;
        let p = t.payload(&day_of(now())).await.expect("payload");
        assert_eq!(p["terms_open_max"], 4);
        assert_eq!(p["terms_open_avg"], 3.0);
        assert_eq!(p["spaces_max"], 3);
        assert_eq!(p["agents_by_kind_max"]["claude"], 4);
        assert_eq!(p["term"]["ghostty"], true);
        assert_eq!(p["phone"], false);
    }

    /// A one-request HTTP server on loopback answering `status`; returns its URL and the body
    /// it received.
    fn hub_answering(status: u16) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!(
            "http://{}/api/telemetry",
            listener.local_addr().expect("addr")
        );
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf).unwrap_or(0);
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= len || n == 0 {
                        let _ = write!(
                            conn,
                            "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        );
                        return text;
                    }
                }
                if n == 0 {
                    return text;
                }
            }
        });
        (url, handle)
    }

    async fn queue_yesterday(t: &Telemetry) -> String {
        let day = day_of(now() - 86_400);
        let d = day.clone();
        t.store
            .run(move |s| {
                s.telemetry_set("id", "6f1c0b8e-2d7a-4c1e-9b3a-5e2f7d9a1c44")?;
                s.telemetry_bump(&d, TERMS_OPENED, 2.0)
            })
            .await
            .expect("queue");
        day
    }

    #[tokio::test]
    // The hub here is plain http, which only a debug build posts to: in release the send fails
    // before it connects, and the hub would wait on `accept` for ever.
    #[cfg_attr(
        not(debug_assertions),
        ignore = "a release build posts over https only"
    )]
    async fn only_a_204_deletes_a_sent_day_and_a_400_three_times_drops_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (status, kept_after_one) in [(204, false), (500, true), (429, true), (301, true)] {
            let (url, hub) = hub_answering(status);
            let t = seen(dir.path(), &url);
            assert_eq!(t.check().await, Standing::On);
            let day = queue_yesterday(&t).await;
            t.send_finished(&day_of(now())).await;
            let request = hub.join().expect("hub");
            assert!(request.starts_with("POST /api/telemetry"), "{request}");
            assert!(
                request.contains("x-forwarded-for: 127.0.0.1")
                    || request.contains("X-Forwarded-For: 127.0.0.1")
            );
            assert!(request.contains(&format!("\"day\":\"{day}\"")), "{request}");
            let left = t.store.run(|s| s.telemetry_days()).await.expect("days");
            assert_eq!(left.contains(&day), kept_after_one, "status {status}");
        }

        let t = seen(dir.path(), URL);
        let day = queue_yesterday(&t).await;
        for round in 1..=3 {
            let (url, hub) = hub_answering(400);
            let t2 =
                Telemetry::with_files(t.store.clone(), url, t.config.clone(), t.noticed.clone());
            t2.check().await;
            t2.send_finished(&day_of(now())).await;
            hub.join().expect("hub");
            let left = t.store.run(|s| s.telemetry_days()).await.expect("days");
            assert_eq!(left.contains(&day), round < 3, "round {round}");
        }
    }

    // Several threads: the test blocks on `accept` while the send runs beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    // The hub here is plain http, which only a debug build posts to: in release the send fails
    // before it connects, and the hub would wait on `accept` for ever.
    #[cfg_attr(
        not(debug_assertions),
        ignore = "a release build posts over https only"
    )]
    async fn an_off_during_a_send_stops_it_and_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A listener that never answers: the send hangs until it is cancelled.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!(
            "http://{}/api/telemetry",
            listener.local_addr().expect("addr")
        );
        let t = seen(dir.path(), &url);
        assert_eq!(t.check().await, Standing::On);
        queue_yesterday(&t).await;
        let sending = {
            let t = t.clone();
            tokio::spawn(async move { t.send_finished(&day_of(now())).await })
        };
        let _held = listener.accept().expect("the send connected");
        let started = std::time::Instant::now();
        t.turn_off().await;
        sending.await.expect("send ended");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancelled, not timed out"
        );
        let left = t.store.run(|s| s.telemetry_days()).await.expect("days");
        assert!(left.is_empty());
    }

    #[tokio::test]
    async fn the_send_moment_is_kept_so_a_short_daemon_life_does_not_push_it_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = seen(dir.path(), URL);
        t.check().await;
        t.send_when_due().await;
        let first = t
            .store
            .run(|s| s.telemetry_get("next_send"))
            .await
            .expect("get");
        let first = first.expect("a moment was picked");
        let at: i64 = first.parse().expect("number");
        assert!(at >= now() && at <= now() + WINDOW);
        // A new daemon on the same store keeps it.
        let t2 = Telemetry::with_files(
            t.store.clone(),
            URL.into(),
            t.config.clone(),
            t.noticed.clone(),
        );
        t2.check().await;
        t2.send_when_due().await;
        let again = t
            .store
            .run(|s| s.telemetry_get("next_send"))
            .await
            .expect("get");
        assert_eq!(again, Some(first));
    }
}
