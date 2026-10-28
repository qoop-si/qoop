//! The "on the clock" panel: the schedules the daemon keeps, on qass's screen, and the form
//! that adds one.
//!
//! Schedules belong to qass, so its screen shows them: each as the line `name  next  when`
//! and under it how the last run went and which tab it opened. The owner pauses, runs now,
//! removes or opens a run from the focused row, and `+` opens one form (a preset for the
//! timing, a time, the agent, the folder and the prompt, with the first run read back before
//! anything is saved). Everything here is a pure function of the rows the daemon sent and of
//! what this client remembers (which row is picked, what is typed), so a test reads the panel
//! instead of looking at one.
//!
//! A write goes out at once and the daemon says `SchedulesChanged` when it is done, which asks
//! for the list again: the list shows the daemon's `next_at`, never this client's guess. The
//! first run in the form is the only local guess, worked out on the local clock the way the
//! daemon will, and it is shown as a guess.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse, ScheduleInfo, ScheduleOp};
use qoop_runtime::schedule::{Cron, Naive, Zone, next_slot, parse_at, system_zone};

use crate::asks::cut_dots;
use crate::keys;
use crate::qass_screen::{Seg, bold, edge, ink, line, seg};
use crate::theme::{self, Role};
use crate::width;

/// How long a word about what just happened stands in place of the key line.
const NOTICE: Duration = Duration::from_secs(6);

/// The id of the request that reads the list, and the prefix of the ones that write it.
pub const READ_ID: &str = "schedules-read";
pub const WRITE_ID: &str = "schedules-write";

/// The rows the form needs besides its chips: both edges, the time, the agent, the folder, the
/// prompt, the first run and the key line.
const FORM_FIXED: usize = 8;
/// The most rows the form takes: its chips on three rows, as in a narrow panel.
pub const FORM_ROWS: usize = FORM_FIXED + 3;
/// The fewest rows a panel under the terminal is worth: borders, a line, the key line.
pub const LIST_MIN_ROWS: usize = 5;
/// The most rows the panel takes under the terminal, the rows it does not leave to qass.
pub const UNDER_MAX_ROWS: usize = 12;
/// The most characters a name, a folder or a typed cron line keeps.
const FIELD_MAX: usize = 200;
/// The most bytes of prompt (the daemon's own limit).
const PROMPT_MAX: usize = qoop_runtime::control::SCHEDULE_PROMPT_MAX;
/// The columns a form label takes, with the blank after it.
const LABEL: usize = 8;
/// The mark before a schedule, and before a paused one. The picked design draws a clock face
/// (U+25F7) and a dotted ring (U+25CC); neither has been measured in a fresh Mac's and a fresh
/// Ubuntu's default fonts (`tests/glyphs.rs`), so the measured ring and dot stand in. Swap
/// the two constants once those are measured and added to `SAFE`.
const ACTIVE: &str = "\u{25cb}";
const PAUSED: &str = "\u{b7}";
/// The caret in a field that has the keys.
const CARET: char = '\u{258f}';

/// How a schedule repeats, as the form's chips say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Once,
    Weekdays,
    EveryDay,
    EveryTwoHours,
    Custom,
    /// A watch: a command checked every few minutes, its new lines start the run.
    OnChange,
}

impl Preset {
    pub const ALL: [Preset; 6] = [
        Preset::Once,
        Preset::Weekdays,
        Preset::EveryDay,
        Preset::EveryTwoHours,
        Preset::Custom,
        Preset::OnChange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Once => "once",
            Preset::Weekdays => "weekdays",
            Preset::EveryDay => "every day",
            Preset::EveryTwoHours => "every 2h",
            Preset::Custom => "custom",
            Preset::OnChange => "on change",
        }
    }
}

/// When a form's schedule fires, as the daemon takes it: a cron line, one moment, or a
/// command to watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Timing {
    Cron(String),
    At(i64),
    Watch(String),
}

/// `H:MM` or `HH:MM` as an hour and a minute.
fn clock(text: &str) -> Result<(u32, u32), String> {
    let wrong = || "the time is HH:MM, like 09:00".to_string();
    let (hour, minute) = text.trim().split_once(':').ok_or_else(wrong)?;
    let digits = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(hour, 1..=2) || !digits(minute, 2..=2) {
        return Err(wrong());
    }
    let (hour, minute) = (
        hour.parse::<u32>().map_err(|_| wrong())?,
        minute.parse::<u32>().map_err(|_| wrong())?,
    );
    if hour > 23 || minute > 59 {
        return Err(wrong());
    }
    Ok((hour, minute))
}

/// The rows the form's chips take when they wrap in `room` columns.
fn chip_rows(room: usize) -> usize {
    let (mut rows, mut used) = (1, 0);
    for preset in Preset::ALL {
        let w = width::width(preset.label()) + 2;
        if used > 0 && used + 1 + w > room {
            rows += 1;
            used = 0;
        }
        used += w + usize::from(used > 0);
    }
    rows
}

/// What a preset, its time (or its text: the custom cron line, the command of on change) and
/// the clock make of the timing: `once` is the next time the clock reads that time, the
/// repeats are the cron lines the daemon names `weekdays 09:00`, `every day 09:00` and
/// `every 2h`, on change the command as typed.
pub fn timing(
    preset: Preset,
    time: &str,
    cron: &str,
    now: i64,
    zone: Zone,
) -> Result<Timing, String> {
    match preset {
        Preset::Once => {
            let (hour, minute) = clock(time)?;
            parse_at(&format!("{hour:02}:{minute:02}"), now, zone).map(Timing::At)
        }
        Preset::Weekdays => clock(time).map(|(h, m)| Timing::Cron(format!("{m} {h} * * 1-5"))),
        Preset::EveryDay => clock(time).map(|(h, m)| Timing::Cron(format!("{m} {h} * * *"))),
        Preset::EveryTwoHours => Ok(Timing::Cron("0 */2 * * *".to_string())),
        Preset::Custom => {
            let text = cron.split_whitespace().collect::<Vec<_>>().join(" ");
            Cron::parse(&text).map_err(|e| format!("{text:?}: {e}"))?;
            Ok(Timing::Cron(text))
        }
        Preset::OnChange => {
            let command = cron.trim();
            if command.is_empty() {
                return Err("write the command to check, like gh issue list".to_string());
            }
            Ok(Timing::Watch(command.to_string()))
        }
    }
}

/// The first moment `timing` fires after `now`, the way the daemon will find it.
pub fn first_run(timing: &Timing, now: i64, zone: Zone) -> Option<i64> {
    match timing {
        Timing::At(at) => Some(*at),
        Timing::Cron(text) => next_slot(&Cron::parse(text).ok()?, now, zone),
        // Not a run: its first check notes what is there, which the form says in words.
        Timing::Watch(_) => None,
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A moment the way the panel says it: the time while it is today, `tomorrow 09:00`, the
/// weekday inside the week (`mon 08:30`), else the date (`oct 24 17:00`).
pub fn moment(at: i64, now: i64, zone: Zone) -> String {
    let (wall, today) = ((zone.local)(at), (zone.local)(now));
    let days = |n: Naive| n.secs().div_euclid(86_400);
    let ahead = days(wall) - days(today);
    let time = wall.clock();
    match ahead {
        0 => time,
        1 => format!("tomorrow {time}"),
        2..=6 => format!("{} {time}", DAYS[(days(wall) + 4).rem_euclid(7) as usize]),
        _ => format!("{} {} {time}", MONTHS[(wall.month - 1) as usize], wall.day),
    }
}

/// The program a launcher's command line starts: its first word that is not `NAME=value`,
/// without its folder.
fn program_of(command: &str) -> &str {
    command
        .split_whitespace()
        .find(|w| !w.contains('='))
        .map_or("", |w| w.rsplit('/').next().unwrap_or(w))
}

/// The agents a form offers: the four a scheduled run can start by name, then the owner's own
/// `[agents]` lines that run one of them (a run is started with a message, which only those
/// take), qass's own pick first.
pub fn agents(launchers: &[crate::config::Launcher], qass: &str) -> Vec<String> {
    let mut all: Vec<String> = crate::config::QASS_AGENTS
        .iter()
        .map(|a| a.to_string())
        .chain(
            launchers
                .iter()
                .filter(|l| crate::config::QASS_AGENTS.contains(&program_of(&l.command)))
                .map(|l| l.name.clone()),
        )
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    all.retain(|a| seen.insert(a.clone()));
    if let Some(at) = all.iter().position(|a| a == qass) {
        all[..=at].rotate_right(1);
    }
    all
}

/// A name for a schedule from the first words of its prompt, in the characters the daemon
/// allows (letters, digits, dots, dashes, underscores, at most 40, never only digits), and
/// not one of `taken`: a number is added when the words were used before.
fn name_of(prompt: &str, taken: &[String]) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(4).collect();
    let mut slug = String::new();
    for c in words.join("-").chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            c.to_ascii_lowercase()
        } else {
            '-'
        };
        if c != '-' || !slug.ends_with('-') {
            slug.push(c);
        }
    }
    // Room is left under the daemon's 40 for the number.
    let slug: String = slug.trim_matches('-').chars().take(32).collect();
    let mut slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "schedule".to_string();
    }
    if slug.bytes().all(|b| b.is_ascii_digit()) {
        slug = format!("run-{slug}");
    }
    let mut name = slug.clone();
    let mut n = 1;
    while taken.contains(&name) {
        n += 1;
        name = format!("{slug}-{n}");
    }
    name
}

/// `path` with a leading `~` made the home folder.
fn expand_home(path: &str, home: Option<&str>) -> String {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => path.to_string(),
    }
}

/// A field of the form, in the order the tab key walks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    When,
    Time,
    Agent,
    Folder,
    Prompt,
}

const FIELDS: [Field; 5] = [
    Field::When,
    Field::Time,
    Field::Agent,
    Field::Folder,
    Field::Prompt,
];

/// The form that adds a schedule.
#[derive(Clone, Debug)]
pub struct Form {
    pub preset: Preset,
    pub field: Field,
    /// `HH:MM`, for the presets that have a time.
    pub time: String,
    /// The cron line of the custom preset.
    pub cron: String,
    /// The command of on change.
    pub check: String,
    pub agents: Vec<String>,
    /// The agent picked, by position.
    pub agent: usize,
    pub folder: String,
    pub prompt: String,
    /// Why the last enter saved nothing.
    pub error: Option<String>,
}

impl Form {
    /// A form for `folder`, the agents it may start (the first is the one picked).
    pub fn new(folder: String, agents: Vec<String>) -> Form {
        Form {
            preset: Preset::Weekdays,
            field: Field::When,
            time: "09:00".to_string(),
            cron: String::new(),
            check: String::new(),
            agents,
            agent: 0,
            folder,
            prompt: String::new(),
            error: None,
        }
    }

    /// Whether the preset has a time or a cron line to type: every two hours has none.
    fn has_time(&self) -> bool {
        self.preset != Preset::EveryTwoHours
    }

    /// The field after (or before) the one with the keys, skipping the time the preset does
    /// not have.
    fn step(&mut self, forward: bool) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0);
        let mut next = at;
        loop {
            next =
                (next + FIELDS.len() + if forward { 1 } else { FIELDS.len() - 1 }) % FIELDS.len();
            if FIELDS[next] != Field::Time || self.has_time() {
                break;
            }
        }
        self.field = FIELDS[next];
    }

    /// The chip or the agent one step left or right.
    fn pick(&mut self, forward: bool) {
        match self.field {
            Field::When => {
                let at = Preset::ALL
                    .iter()
                    .position(|p| *p == self.preset)
                    .unwrap_or(0);
                let n = Preset::ALL.len();
                self.preset = Preset::ALL[(at + if forward { 1 } else { n - 1 }) % n];
            }
            Field::Agent if !self.agents.is_empty() => {
                let n = self.agents.len();
                self.agent = (self.agent + if forward { 1 } else { n - 1 }) % n;
            }
            _ => {}
        }
        self.error = None;
    }

    /// The text field with the keys: the time or cron line, the folder or the prompt.
    fn text(&mut self) -> Option<&mut String> {
        match self.field {
            Field::Time if self.preset == Preset::Custom => Some(&mut self.cron),
            Field::Time if self.preset == Preset::OnChange => Some(&mut self.check),
            Field::Time if self.has_time() => Some(&mut self.time),
            Field::Folder => Some(&mut self.folder),
            Field::Prompt => Some(&mut self.prompt),
            _ => None,
        }
    }

    /// The timing as typed, or why it is not one.
    pub fn timing(&self, now: i64, zone: Zone) -> Result<Timing, String> {
        let text = if self.preset == Preset::OnChange {
            &self.check
        } else {
            &self.cron
        };
        timing(self.preset, &self.time, text, now, zone)
    }

    /// The write that adds what the form says, or why it cannot yet.
    pub fn request(
        &self,
        now: i64,
        zone: Zone,
        home: Option<&str>,
        taken: &[String],
    ) -> Result<ScheduleOp, String> {
        let timing = self.timing(now, zone)?;
        let prompt = self.prompt.trim();
        if prompt.is_empty() {
            return Err("write what the agent should do".to_string());
        }
        if prompt.len() > PROMPT_MAX {
            return Err(format!("the prompt is at most {PROMPT_MAX} bytes"));
        }
        let cwd = expand_home(self.folder.trim(), home);
        if !cwd.starts_with('/') {
            return Err("the folder starts with / or ~".to_string());
        }
        let agent = self
            .agents
            .get(self.agent)
            .cloned()
            .ok_or("there is no agent to start")?;
        let (cron, at, watch) = match timing {
            Timing::Cron(text) => (Some(text), None, None),
            Timing::At(at) => (None, Some(at), None),
            Timing::Watch(command) => (None, None, Some(command)),
        };
        Ok(ScheduleOp::Add {
            name: name_of(prompt, taken),
            cron,
            at,
            watch,
            agent: Some(agent),
            cwd,
            space: None,
            prompt: prompt.to_string(),
        })
    }
}

/// What a key did, for the loop that owns the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Taken: repaint.
    Stay,
    /// The keyboard goes back to where it was before the panel.
    Back,
    /// Go to this terminal's tab.
    Jump(u32),
    /// `+`: the owner wants the form, which the caller fills with the folder to start from.
    Add,
    /// Not the panel's: the prefix, a mouse report or a detach, for the ordinary path.
    Pass(Vec<u8>),
}

/// The panel as this client keeps it.
pub struct Pane {
    /// The keyboard is in the panel rather than in qass's chat.
    pub focus: bool,
    /// Every schedule, oldest first, as the daemon last said.
    pub list: Vec<ScheduleInfo>,
    /// The daemon has answered a read once.
    pub loaded: bool,
    /// The daemon predates schedules: the panel is never shown.
    pub unsupported: bool,
    /// The read is owed: a change event came, or the first read has not been asked.
    pub stale: bool,
    /// The owner asked for the panel while nothing is on the clock, to reach `+`.
    pub raised: bool,
    /// The form, while one is open.
    pub form: Option<Form>,
    selected: usize,
    /// The schedule a `x` asked to remove, waiting for its `Y`.
    confirm: Option<i64>,
    /// The schedule the daemon made last: the list picks it when it arrives.
    pick: Option<i64>,
    /// The form just saved and the request carrying it: the daemon may still refuse (a folder
    /// that is not there), and then the form comes back with the reason, words and all.
    saving: Option<(String, Form)>,
    outbox: Vec<ControlRequest>,
    /// Writes sent and not answered, by request id, with what to say once they are.
    sent: BTreeMap<String, String>,
    next_id: u32,
    notice: Option<String>,
    notice_until: Option<Instant>,
    /// The zone every time here is read in: the machine's, and a test's own.
    pub zone: Zone,
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("focus", &self.focus)
            .field("list", &self.list.len())
            .field("form", &self.form.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Pane {
    fn default() -> Self {
        Self::new()
    }
}

impl Pane {
    /// A panel not yet read: the first read is owed.
    pub fn new() -> Pane {
        Pane {
            focus: false,
            list: Vec::new(),
            loaded: false,
            unsupported: false,
            stale: true,
            raised: false,
            form: None,
            selected: 0,
            confirm: None,
            pick: None,
            saving: None,
            outbox: Vec::new(),
            sent: BTreeMap::new(),
            next_id: 0,
            notice: None,
            notice_until: None,
            zone: system_zone(),
        }
    }

    /// Whether the panel is on screen: the form is open, the owner asked for it, or something
    /// is on the clock. An old daemon never shows it.
    pub fn shown(&self) -> bool {
        !self.unsupported && (self.form.is_some() || self.raised || !self.list.is_empty())
    }

    /// The rows the panel wants under the terminal in a pane `cols` wide, none while it is not
    /// shown. The form wants as many as its chips wrap to.
    pub fn rows_wanted(&self, cols: usize) -> usize {
        if !self.shown() {
            return 0;
        }
        if self.form.is_some() {
            return FORM_FIXED + chip_rows(cols.saturating_sub(4 + LABEL));
        }
        let body: usize = self
            .list
            .iter()
            .map(|s| if s.paused { 1 } else { 2 })
            .sum::<usize>()
            .max(2);
        (2 + body + 1).clamp(LIST_MIN_ROWS, UNDER_MAX_ROWS)
    }

    /// The schedule with the pick, if any.
    pub fn picked(&self) -> Option<&ScheduleInfo> {
        self.list
            .get(self.selected.min(self.list.len().saturating_sub(1)))
    }

    /// Take a fresh read: the pick stays on its schedule while it is there, else on the one
    /// the daemon made last.
    pub fn take(&mut self, list: Vec<ScheduleInfo>) {
        let keep = self.pick.take().or_else(|| self.picked().map(|s| s.id));
        self.list = list;
        self.loaded = true;
        self.unsupported = false;
        self.selected = keep
            .and_then(|id| self.list.iter().position(|s| s.id == id))
            .unwrap_or(self.selected)
            .min(self.list.len().saturating_sub(1));
        if self
            .confirm
            .is_some_and(|id| !self.list.iter().any(|s| s.id == id))
        {
            self.confirm = None;
        }
        // Nothing is on the clock and he did not ask for the panel: it goes, form or not.
        if self.list.is_empty() && !self.raised && self.form.is_none() {
            self.focus = false;
        }
    }

    /// The read to send, when one is owed and none is in flight.
    pub fn read_request(&mut self) -> Option<ControlRequest> {
        if !self.stale {
            return None;
        }
        self.stale = false;
        Some(ControlRequest::ScheduleRead {
            id: READ_ID.to_string(),
        })
    }

    /// The daemon's answer to a request of the panel's. True when the screen changed.
    pub fn answered(&mut self, response: &ControlResponse) -> bool {
        match response {
            ControlResponse::Schedules { id, schedules } if id == READ_ID => {
                self.take(schedules.clone());
                true
            }
            ControlResponse::ScheduleWritten { id, schedule } if self.sent.contains_key(id) => {
                let said = self.sent.remove(id).unwrap_or_default();
                self.saving = None;
                if said.starts_with("added") {
                    self.pick = Some(*schedule);
                }
                self.say(said);
                self.stale = true;
                true
            }
            ControlResponse::Failed { id, reason } if self.sent.contains_key(id) => {
                self.sent.remove(id);
                // A form the daemon refused comes back as it was, with the reason on it.
                if let Some((_, mut form)) = self.saving.take_if(|(sent, _)| sent == id) {
                    form.error = Some(reason.clone());
                    self.form = Some(form);
                } else {
                    self.say(reason.clone());
                }
                self.stale = true;
                true
            }
            ControlResponse::Failed { id, reason }
                // Only an older daemon hides the panel: a read that timed out keeps what was shown.
                if (id == READ_ID || id.is_empty())
                    && qoop_runtime::control::too_old_for(reason, Some("ScheduleRead")) =>
            {
                self.unsupported = true;
                self.loaded = false;
                self.focus = false;
                self.form = None;
                true
            }
            _ => false,
        }
    }

    /// The moment the panel next has something to do by itself: a write to send now or a
    /// notice to let go.
    pub fn wake_at(&self) -> Option<Instant> {
        match (self.outbox.is_empty(), self.notice_until) {
            (false, _) => Some(Instant::now()),
            (true, until) => until,
        }
    }

    /// Say something in place of the key line, for a few seconds.
    pub(crate) fn say(&mut self, what: impl Into<String>) {
        self.notice = Some(what.into());
        self.notice_until = Some(Instant::now() + NOTICE);
    }

    /// A tick: the writes to send, and whether the screen changed.
    pub fn tick(&mut self, now: Instant) -> (Vec<ControlRequest>, bool) {
        let mut moved = false;
        if self.notice_until.is_some_and(|until| now >= until) {
            self.notice = None;
            self.notice_until = None;
            moved = true;
        }
        (std::mem::take(&mut self.outbox), moved)
    }

    /// The keyboard leaves the panel: a question half asked is dropped, an empty panel he
    /// raised goes away.
    pub fn release(&mut self) {
        self.focus = false;
        self.confirm = None;
        if self.list.is_empty() {
            self.raised = false;
        }
    }

    /// Write `action`, and say `said` once the daemon has done it. Answers the request's id.
    fn write(&mut self, action: ScheduleOp, said: String) -> String {
        self.next_id += 1;
        let id = format!("{WRITE_ID}-{}", self.next_id);
        self.sent.insert(id.clone(), said);
        self.outbox.push(ControlRequest::ScheduleWrite {
            id: id.clone(),
            terminal: None,
            token: None,
            action: Box::new(action),
        });
        self.notice = None;
        self.notice_until = None;
        id
    }

    /// Open the form for `folder`, with the agents the owner's config and qass's pick say.
    pub fn open_form(&mut self, folder: String) {
        let read = crate::config::read();
        self.form = Some(Form::new(folder, agents(&read.agents, &read.qass_agent)));
        self.notice = None;
        self.notice_until = None;
        self.confirm = None;
    }

    /// Save the form: the write goes out and the form closes, or the reason it did not stays
    /// on the form.
    fn save(&mut self, now: i64, home: Option<&str>) {
        let Some(form) = &mut self.form else {
            return;
        };
        let taken: Vec<String> = self.list.iter().map(|s| s.name.clone()).collect();
        match form.request(now, self.zone, home, &taken) {
            Ok(action) => {
                let name = match &action {
                    ScheduleOp::Add { name, .. } => name.clone(),
                    _ => String::new(),
                };
                let form = self.form.take();
                let id = self.write(action, format!("added {name}"));
                self.saving = form.map(|form| (id, form));
            }
            Err(reason) => form.error = Some(reason),
        }
    }

    /// Move the pick by one.
    fn step(&mut self, down: bool) {
        let last = self.list.len().saturating_sub(1);
        let at = self.selected.min(last);
        self.select(if down {
            (at + 1).min(last)
        } else {
            at.saturating_sub(1)
        });
    }

    /// Pick schedule number `at`.
    pub fn select(&mut self, at: usize) {
        if at < self.list.len() {
            self.selected = at;
            self.confirm = None;
            self.notice = None;
            self.notice_until = None;
        }
    }

    /// A key, or the bytes of several. `prefix` is the interface's prefix byte, which with a
    /// mouse report or a detach is not the panel's and goes back.
    pub fn key(&mut self, bytes: &[u8], prefix: u8) -> Out {
        if bytes.first() == Some(&prefix) || bytes.starts_with(b"\x1b[<") || bytes == b"\x11" {
            return Out::Pass(bytes.to_vec());
        }
        let mut out = Out::Stay;
        for key in keys_of(bytes) {
            let did = if self.form.is_some() {
                self.form_key(key)
            } else {
                self.list_key(key)
            };
            if did != Out::Stay {
                out = did;
                break;
            }
        }
        out
    }

    fn list_key(&mut self, key: Key) -> Out {
        if let Some(id) = self.confirm.take() {
            // Only a capital Y removes; whatever else comes keeps it and is spent on that.
            if key == Key::Char('Y')
                && let Some(s) = self.list.iter().find(|s| s.id == id)
            {
                let said = format!("removed {}", s.name);
                self.write(ScheduleOp::Remove { schedule: id }, said);
            }
            return Out::Stay;
        }
        match key {
            Key::Esc | Key::Tab => return Out::Back,
            Key::Down | Key::Char('j') => self.step(true),
            Key::Up | Key::Char('k') => self.step(false),
            Key::Char('+') => return Out::Add,
            Key::Enter => {
                return match self.picked().map(|s| (s.last_terminal, &s.name)) {
                    Some((Some(t), _)) => u32::try_from(t).map_or(Out::Stay, Out::Jump),
                    Some((None, name)) => {
                        let said = format!("{name} has not run yet");
                        self.say(said);
                        Out::Stay
                    }
                    None => Out::Stay,
                };
            }
            Key::Char(' ') => {
                if let Some(s) = self.picked() {
                    let (id, name) = (s.id, s.name.clone());
                    if s.paused {
                        self.write(
                            ScheduleOp::Resume { schedule: id },
                            format!("resumed {name}"),
                        );
                    } else {
                        self.write(ScheduleOp::Pause { schedule: id }, format!("paused {name}"));
                    }
                }
            }
            Key::Char('r') => {
                if let Some(s) = self.picked() {
                    let (id, name) = (s.id, s.name.clone());
                    self.write(ScheduleOp::Run { schedule: id }, format!("started {name}"));
                }
            }
            Key::Char('x') => self.confirm = self.picked().map(|s| s.id),
            _ => {}
        }
        Out::Stay
    }

    fn form_key(&mut self, key: Key) -> Out {
        let Some(form) = &mut self.form else {
            return Out::Stay;
        };
        match key {
            Key::Esc => self.form = None,
            Key::Tab | Key::Down => form.step(true),
            Key::BackTab | Key::Up => form.step(false),
            Key::Right => form.pick(true),
            Key::Left => form.pick(false),
            Key::Enter => self.save(
                qoop_runtime::store::now(),
                std::env::var("HOME").ok().as_deref(),
            ),
            Key::Char(c) => type_into(form, c),
            Key::Other => {}
        }
        Out::Stay
    }

    /// A click on something the view drew.
    pub fn click(&mut self, hit: Hit) -> Out {
        match hit {
            Hit::Row(at) => self.select(at),
            Hit::Add => return Out::Add,
            Hit::Chip(at) => {
                if let Some(form) = &mut self.form {
                    form.preset = Preset::ALL[at.min(Preset::ALL.len() - 1)];
                    form.field = Field::When;
                    form.error = None;
                }
            }
            Hit::Field(field) => {
                if let Some(form) = &mut self.form {
                    form.field = field;
                }
            }
        }
        Out::Stay
    }
}

/// One key of the bytes a read carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Esc,
    Tab,
    BackTab,
    Enter,
    Up,
    Down,
    Left,
    Right,
    Char(char),
    /// An escape sequence that is nothing here (a delete, a paste's brackets).
    Other,
}

/// The keys in `bytes`, which may be several (a fast typist, a paste, a test): arrows and
/// shift-tab as the sequences a terminal sends, a lone escape as escape, the rest as the
/// characters they spell.
fn keys_of(bytes: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut text = Vec::new();
    let flush = |text: &mut Vec<u8>, out: &mut Vec<Key>| {
        out.extend(String::from_utf8_lossy(text).chars().map(Key::Char));
        text.clear();
    };
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (key, used) = match rest {
            [b'\t', ..] => (Key::Tab, 1),
            [b'\r' | b'\n', ..] => (Key::Enter, 1),
            [0x1b] => (Key::Esc, 1),
            [0x1b, b'[' | b'O', b'A', ..] => (Key::Up, 3),
            [0x1b, b'[' | b'O', b'B', ..] => (Key::Down, 3),
            [0x1b, b'[' | b'O', b'C', ..] => (Key::Right, 3),
            [0x1b, b'[' | b'O', b'D', ..] => (Key::Left, 3),
            [0x1b, b'[', b'Z', ..] => (Key::BackTab, 3),
            // Some other sequence: not text, and not worth reading as letters.
            [0x1b, ..] => (Key::Other, rest.len()),
            _ => {
                text.push(rest[0]);
                i += 1;
                continue;
            }
        };
        flush(&mut text, &mut out);
        out.push(key);
        i += used;
    }
    flush(&mut text, &mut out);
    out
}

/// One typed character into the field with the keys.
fn type_into(form: &mut Form, c: char) {
    form.error = None;
    let on_change = form.preset == Preset::OnChange;
    let (is_time, is_long) = (
        form.field == Field::Time && form.preset != Preset::Custom && !on_change,
        form.field == Field::Prompt || (form.field == Field::Time && on_change),
    );
    let Some(text) = form.text() else {
        return;
    };
    match c {
        '\u{7f}' | '\u{8}' => {
            text.pop();
        }
        '\u{15}' => text.clear(),
        c if c.is_control() => {}
        c if is_time => {
            if (c.is_ascii_digit() || c == ':') && text.len() < 5 {
                text.push(c);
            }
        }
        c if is_long => {
            if text.len() + c.len_utf8() <= PROMPT_MAX {
                text.push(c);
            }
        }
        c => {
            if text.chars().count() < FIELD_MAX {
                text.push(c);
            }
        }
    }
}

/// What a click in the panel landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A schedule, by position.
    Row(usize),
    /// `+ add` on the top edge.
    Add,
    /// A chip of the form, by position in [`Preset::ALL`].
    Chip(usize),
    /// A field of the form.
    Field(Field),
}

/// A cell range on one row of the panel that a click answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub row: usize,
    pub x0: usize,
    pub x1: usize,
    pub hit: Hit,
}

/// What painting needs besides the pane: the clock and the zone it is read in.
#[derive(Clone, Copy)]
pub struct Ctx {
    pub now_unix: i64,
    pub zone: Zone,
    /// A box is open over the screen, which fades everything behind it.
    pub fade: bool,
}

/// The panel painted: one string per row, each exactly as wide as asked, and where a click
/// lands.
#[derive(Clone, Debug)]
pub struct View {
    pub rows: Vec<String>,
    pub targets: Vec<Target>,
}

/// What the first line of a schedule says on the right: when it is next, and the daemon's
/// words for the timing.
fn next_words(s: &ScheduleInfo, ctx: &Ctx) -> (String, Role) {
    match (s.paused, s.next_at) {
        (true, _) => ("paused".to_string(), Role::FaintText),
        (false, Some(at)) => (moment(at, ctx.now_unix, ctx.zone), Role::WorkingText),
        (false, None) => ("done".to_string(), Role::FaintText),
    }
}

/// How the last run went, `last started 09:00 → tab 12`, in the ink of its result.
fn last_segs(s: &ScheduleInfo, ctx: &Ctx) -> Vec<Seg> {
    let mut out = vec![seg(Role::Text, "   "), seg(Role::FaintText, "last ")];
    let Some(result) = s.last_result.as_deref() else {
        out.push(seg(Role::FaintText, "not run yet"));
        return out;
    };
    let said = result.replacen(": ", ", ", 1);
    let (role, mark) = match result.split(':').next().unwrap_or_default() {
        "started" => (Role::Done, "\u{2713} "),
        "claimed" => (Role::WorkingText, ""),
        "skipped" => (Role::Sleeping, ""),
        "failed" => (Role::FailedText, ""),
        _ => (Role::Detail, ""),
    };
    let said = if said == "claimed" {
        "starting".to_string()
    } else {
        said
    };
    out.push(seg(role, format!("{mark}{said}")));
    if result == "started"
        && let Some(at) = s.last_at
    {
        out.push(seg(
            Role::Detail,
            format!(" {}", moment(at, ctx.now_unix, ctx.zone)),
        ));
    }
    if let Some(tab) = s.last_terminal {
        out.push(seg(Role::Detail, format!(" \u{2192} tab {tab}")));
    }
    out
}

/// The columns the list's rows line up on: the names, then when each is next. The words for
/// the timing take what is left, so the eye reads a row left to right in a short stretch even
/// in a wide panel.
struct Cols {
    name: usize,
    next: usize,
}

/// The columns for `list` in `cw` columns: the longest name and the longest "next" word, the
/// name giving way first when the row is too narrow for both.
fn cols_of(list: &[ScheduleInfo], ctx: &Ctx, cw: usize) -> Cols {
    let next = list
        .iter()
        .map(|s| width::width(&next_words(s, ctx).0))
        .max()
        .unwrap_or(0);
    let longest = list
        .iter()
        .map(|s| width::width(&s.name))
        .max()
        .unwrap_or(0);
    Cols {
        name: longest.min(cw.saturating_sub(MARK + 2 + next)).max(1),
        next,
    }
}

/// The marker, the glyph and the blank after them, in columns.
const MARK: usize = 4;
/// The fewest columns the daemon's words for the timing are worth.
const WHEN_MIN: usize = 10;

/// The rows one schedule takes: `<mark> name  next  when`, then how the last run went (a paused
/// schedule says only that it is paused, faint).
fn block(s: &ScheduleInfo, ctx: &Ctx, cw: usize, cols: &Cols, marked: bool) -> Vec<Vec<Seg>> {
    let (next, next_role) = next_words(s, ctx);
    let glyph = if s.paused { PAUSED } else { ACTIVE };
    let mark = if marked {
        bold(Role::Brand, "\u{25b8}")
    } else {
        seg(Role::Text, " ")
    };
    let (glyph_role, name_role) = match (s.paused, marked) {
        (true, _) => (Role::FaintText, Role::FaintText),
        (false, true) => (Role::Working, Role::Focused),
        (false, false) => (Role::Working, Role::Text),
    };
    let name = cut_dots(&s.name, cols.name);
    let pad = cols.name.saturating_sub(width::width(&name));
    let next_pad = cols.next.saturating_sub(width::width(&next));
    let mut first = vec![
        mark,
        seg(glyph_role, glyph),
        seg(Role::Text, " "),
        seg(name_role, name),
        seg(Role::Text, " ".repeat(pad + 2)),
        seg(next_role, next),
    ];
    // The daemon's words for the timing, in what is left of the row.
    let left = cw.saturating_sub(MARK + cols.name + 2 + cols.next + 2);
    if !s.paused && left >= WHEN_MIN {
        first.push(seg(Role::Text, " ".repeat(next_pad + 2)));
        first.push(seg(Role::Detail, cut_dots(&s.when, left)));
    }
    let mut rows = vec![first];
    if !s.paused {
        rows.push(last_segs(s, ctx));
    }
    rows
}

/// What the closing line says: the question being asked, the daemon's last word, the form's
/// refusal, or the keys.
fn bottom_line(pane: &Pane, cw: usize) -> Vec<Seg> {
    if let Some(error) = pane.form.as_ref().and_then(|f| f.error.as_ref()) {
        return vec![seg(Role::FailedText, cut_dots(error, cw))];
    }
    if let Some(id) = pane.confirm
        && let Some(s) = pane.list.iter().find(|s| s.id == id)
    {
        let ask = format!("Y removes {}, any other key keeps it", s.name);
        return vec![seg(Role::Blocked, cut_dots(&ask, cw))];
    }
    if let Some(n) = &pane.notice {
        return vec![seg(Role::Detail, cut_dots(n, cw))];
    }
    let words = if pane.form.is_some() {
        "tab next  \u{2190}\u{2192} pick  enter save  esc cancel"
    } else if pane.focus {
        "j k move  enter open  space pause  r run  x remove  + new"
    } else {
        "ctrl-a C focuses this"
    };
    vec![seg(Role::FaintText, cut_dots(&keys::named(words), cw))]
}

/// `text` as its last `room` columns behind a `…`, so the letter being typed is on screen.
fn tail(text: &str, room: usize) -> String {
    if width::width(text) <= room {
        return text.to_string();
    }
    let mut kept = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = width::columns(c);
        if used + w > room {
            break;
        }
        used += w;
        kept.push(c);
    }
    std::iter::once('\u{2026}')
        .chain(kept.into_iter().rev())
        .collect()
}

/// The panel's rows for a box `cols x rows` (border included).
pub fn view(pane: &Pane, ctx: &Ctx, cols: usize, rows: usize) -> View {
    let fade = ctx.fade;
    let cw = cols.saturating_sub(4);
    let inner_rows = rows.saturating_sub(2);
    let rule = ink(Role::Rule, fade);
    let wrap = |segs: &[Seg]| -> String {
        format!(
            "{rule}\u{2502}{} {} {rule}\u{2502}{}",
            theme::RESET,
            line(segs, cw, fade),
            theme::RESET
        )
    };
    let blank = || wrap(&[]);
    let mut targets = Vec::new();

    let title = if pane.form.is_some() {
        "new schedule"
    } else {
        "schedules"
    };
    let tag = (pane.form.is_none()).then_some("+ add");
    // Where the tag is, when the edge keeps it: before the last corner and a rule cell.
    let tag_w = tag.map_or(0, |t| 3 + width::width(t));
    let title_w = 3 + width::width(title);
    if let Some(t) = tag.filter(|_| 2 + title_w + tag_w <= cols) {
        targets.push(Target {
            row: 0,
            x0: cols - 3 - width::width(t),
            x1: cols - 3,
            hit: Hit::Add,
        });
    }
    let mut out = vec![edge(cols, ('\u{256d}', '\u{256e}'), Some(title), tag, fade)];
    // The rows between the edges, less the key line.
    let content = inner_rows.saturating_sub(1);
    let mut body: Vec<String> = Vec::new();

    if let Some(form) = &pane.form {
        form_rows(form, ctx, cw, &wrap, &mut body, &mut targets);
    } else if pane.unsupported {
        body.push(wrap(&[seg(Role::Detail, "restart qoop for schedules")]));
    } else if !pane.loaded {
        body.push(wrap(&[seg(Role::FaintText, "reading the schedules")]));
    } else if pane.list.is_empty() {
        body.push(wrap(&[seg(Role::Detail, "Nothing scheduled.")]));
        body.push(wrap(&[seg(Role::FaintText, "+ adds one")]));
    } else {
        let cols = cols_of(&pane.list, ctx, cw);
        let blocks: Vec<Vec<Vec<Seg>>> = pane
            .list
            .iter()
            .enumerate()
            .map(|(i, s)| block(s, ctx, cw, &cols, pane.focus && i == pane.selected))
            .collect();
        let picked = pane.selected.min(blocks.len() - 1);
        // Scrolled to keep the pick whole: the first block shown is the first from which the
        // pick still fits.
        let ends: Vec<usize> = blocks
            .iter()
            .scan(0, |at, b| {
                *at += b.len();
                Some(*at)
            })
            .collect();
        let mut top = 0;
        while top < picked && ends[picked] - if top == 0 { 0 } else { ends[top - 1] } > content {
            top += 1;
        }
        for (i, rows_of) in blocks.iter().enumerate().skip(top) {
            for segs in rows_of {
                targets.push(Target {
                    row: 1 + body.len(),
                    x0: 2,
                    x1: 2 + cw,
                    hit: Hit::Row(i),
                });
                body.push(wrap(segs));
            }
        }
    }

    body.truncate(content);
    while body.len() < content {
        body.push(blank());
    }
    out.extend(body);
    out.push(wrap(&bottom_line(pane, cw)));
    out.push(edge(cols, ('\u{2570}', '\u{256f}'), None, None, fade));
    out.truncate(rows);
    // A target below the clipped rows is no target.
    targets.retain(|t| t.row <= content);
    View { rows: out, targets }
}

/// The form's rows: the chips (wrapped to the width), the time, the agent, the folder, the
/// prompt, and the first run read back.
fn form_rows(
    form: &Form,
    ctx: &Ctx,
    cw: usize,
    wrap: &impl Fn(&[Seg]) -> String,
    out: &mut Vec<String>,
    targets: &mut Vec<Target>,
) {
    let label = |text: &str, field: Field| {
        let role = if form.field == field {
            Role::Focused
        } else {
            Role::FaintText
        };
        let text = format!("{text:<LABEL$}");
        if form.field == field {
            bold(role, text)
        } else {
            seg(role, text)
        }
    };
    let blank_label = || seg(Role::Text, " ".repeat(LABEL));
    let room = cw.saturating_sub(LABEL);
    let field_row =
        |out: &mut Vec<String>, targets: &mut Vec<Target>, field: Field, segs: Vec<Seg>| {
            targets.push(Target {
                row: 1 + out.len(),
                x0: 2,
                x1: 2 + cw,
                hit: Hit::Field(field),
            });
            out.push(wrap(&segs));
        };

    // The chips, a row of them as wide as the panel lets, the label on the first.
    let mut row = vec![label("when", Field::When)];
    let mut used = 0;
    for (i, preset) in Preset::ALL.iter().enumerate() {
        let chip = format!("[{}]", preset.label());
        let w = width::width(&chip);
        if used > 0 && used + 1 + w > room {
            field_row(
                out,
                targets,
                Field::When,
                std::mem::replace(&mut row, vec![blank_label()]),
            );
            used = 0;
        }
        if used > 0 {
            row.push(seg(Role::Text, " "));
            used += 1;
        }
        // The chip's own cells are found before the row's, which a click on the gap falls to.
        let at = 2 + LABEL + used;
        targets.push(Target {
            row: 1 + out.len(),
            x0: at,
            x1: at + w,
            hit: Hit::Chip(i),
        });
        row.push(if *preset == form.preset {
            bold(Role::Brand, chip)
        } else {
            seg(Role::Detail, chip)
        });
        used += w;
    }
    field_row(out, targets, Field::When, row);

    let caret = |on: bool| {
        seg(
            Role::Brand,
            if on { CARET.to_string() } else { String::new() },
        )
    };
    // The time, the cron line of the custom chip, or the command of on change.
    let mut row = vec![label(
        match form.preset {
            Preset::Custom => "cron",
            Preset::OnChange => "check",
            _ => "time",
        },
        Field::Time,
    )];
    match (form.preset, form.field == Field::Time) {
        (Preset::EveryTwoHours, _) => row.push(seg(Role::FaintText, "on the hour")),
        (Preset::Custom, on) => {
            row.extend(typed(&form.cron, "m h dom mon dow", room, on, &caret));
        }
        (Preset::OnChange, on) => {
            row.extend(typed(
                &form.check,
                "a command, like gh issue list",
                room,
                on,
                &caret,
            ));
        }
        (_, on) => {
            row.extend(typed(&form.time, "HH:MM", room, on, &caret));
        }
    }
    field_row(out, targets, Field::Time, row);

    // The agent: the one picked, then the others it can turn to.
    let mut row = vec![label("agent", Field::Agent)];
    let (on, n) = (form.field == Field::Agent, form.agents.len());
    for (i, name) in form
        .agents
        .iter()
        .cycle()
        .skip(form.agent)
        .take(n)
        .enumerate()
    {
        match i {
            0 => {
                row.push(bold(Role::Text, name.clone()));
                if on && n > 1 {
                    row.push(seg(Role::FaintText, "  \u{2039}"));
                }
            }
            _ => row.push(seg(Role::FaintText, format!(" {name}"))),
        }
    }
    if on && n > 1 {
        row.push(seg(Role::FaintText, " \u{203a}"));
    }
    field_row(out, targets, Field::Agent, row);

    let mut row = vec![label("folder", Field::Folder)];
    row.extend(typed(
        &form.folder,
        "a folder",
        room,
        form.field == Field::Folder,
        &caret,
    ));
    field_row(out, targets, Field::Folder, row);

    let mut row = vec![label("prompt", Field::Prompt)];
    row.extend(typed(
        &form.prompt,
        "what the agent should do",
        room,
        form.field == Field::Prompt,
        &caret,
    ));
    field_row(out, targets, Field::Prompt, row);

    // The first run, worked out on the local clock; the list shows the daemon's after saving.
    // A watch has no first run: its first check notes what is there.
    let first = if form.preset == Preset::OnChange {
        vec![seg(
            Role::FaintText,
            "checked every 5m; new lines start the agent",
        )]
    } else {
        match form
            .timing(ctx.now_unix, ctx.zone)
            .map(|t| first_run(&t, ctx.now_unix, ctx.zone))
        {
            Ok(Some(at)) => vec![
                seg(Role::FaintText, "first run "),
                seg(Role::WorkingText, moment(at, ctx.now_unix, ctx.zone)),
            ],
            Ok(None) => vec![seg(Role::FaintText, "first run never")],
            Err(_) => vec![seg(Role::FaintText, "first run after the time is set")],
        }
    };
    out.push(wrap(&first));
}

/// A field's text in its ink and the caret while it has the keys, or its hint faint when
/// empty.
fn typed(text: &str, hint: &str, room: usize, on: bool, caret: &impl Fn(bool) -> Seg) -> Vec<Seg> {
    let room = room.saturating_sub(usize::from(on));
    if text.is_empty() {
        return vec![caret(on), seg(Role::FaintText, cut_dots(hint, room))];
    }
    vec![seg(Role::Text, tail(text, room)), caret(on)]
}

/// The panel's rectangle in the pane, for a click: its place and what was painted there.
pub fn hit_at(view: &View, rect: crate::layout::Rect, row: usize, col: usize) -> Option<Hit> {
    let (r, x) = (row.checked_sub(rect.y)?, col.checked_sub(rect.x)?);
    view.targets
        .iter()
        .find(|t| t.row == r && x >= t.x0 && x < t.x1)
        .map(|t| t.hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A zone that reads the clock as UTC, so a test knows what time it is.
    fn utc() -> Zone {
        Zone {
            local: Naive::from_secs,
            unix: Naive::secs,
        }
    }

    /// 2026-10-22 (a Thursday) at `hour`:`minute` UTC.
    fn at(hour: u32, minute: u32) -> i64 {
        Naive::new(2026, 10, 22, hour, minute).secs()
    }

    fn info(id: i64, name: &str) -> ScheduleInfo {
        ScheduleInfo {
            id,
            name: name.to_string(),
            cron: Some("0 9 * * 1-5".to_string()),
            at: None,
            watch: None,
            agent: "claude".to_string(),
            cwd: "/tmp".to_string(),
            space: None,
            prompt: "review".to_string(),
            paused: false,
            created_at: at(7, 0),
            last_slot: None,
            last_at: None,
            last_result: None,
            last_terminal: None,
            next_at: Some(at(9, 0)),
            when: "weekdays 09:00".to_string(),
        }
    }

    fn ctx() -> Ctx {
        Ctx {
            now_unix: at(8, 0),
            zone: utc(),
            fade: false,
        }
    }

    fn pane(list: Vec<ScheduleInfo>) -> Pane {
        let mut pane = Pane::new();
        pane.zone = utc();
        pane.take(list);
        pane
    }

    fn painted(pane: &Pane, cols: usize, rows: usize) -> Vec<String> {
        view(pane, &ctx(), cols, rows)
            .rows
            .iter()
            .map(|r| strip(r))
            .collect()
    }

    #[test]
    fn a_moment_is_the_time_today_then_the_day() {
        let now = at(8, 0);
        assert_eq!(moment(at(9, 0), now, utc()), "09:00");
        assert_eq!(moment(at(9, 0) + 86_400, now, utc()), "tomorrow 09:00");
        // Thursday 22 oct: three days on is a Sunday.
        assert_eq!(moment(at(8, 30) + 3 * 86_400, now, utc()), "sun 08:30");
        assert_eq!(moment(at(17, 0) + 9 * 86_400, now, utc()), "oct 31 17:00");
    }

    #[test]
    fn each_chip_becomes_the_timing_the_daemon_names() {
        let now = at(8, 0);
        let go = |p, time: &str, cron: &str| timing(p, time, cron, now, utc());
        assert_eq!(
            go(Preset::Weekdays, "9:05", ""),
            Ok(Timing::Cron("5 9 * * 1-5".to_string()))
        );
        assert_eq!(
            go(Preset::EveryDay, "09:00", ""),
            Ok(Timing::Cron("0 9 * * *".to_string()))
        );
        assert_eq!(
            go(Preset::EveryTwoHours, "", ""),
            Ok(Timing::Cron("0 */2 * * *".to_string()))
        );
        assert_eq!(
            go(Preset::Custom, "", "  30   6 * *  1 "),
            Ok(Timing::Cron("30 6 * * 1".to_string()))
        );
        // Once is the next time the clock reads that time: later today, else tomorrow.
        assert_eq!(go(Preset::Once, "17:00", ""), Ok(Timing::At(at(17, 0))));
        assert_eq!(
            go(Preset::Once, "07:00", ""),
            Ok(Timing::At(at(7, 0) + 86_400))
        );
    }

    #[test]
    fn a_time_or_cron_line_that_is_not_one_is_refused_with_a_reason() {
        let now = at(8, 0);
        for bad in ["", "9", "25:00", "09:60", "9:5", "ab:cd", "09:00:00"] {
            let said = timing(Preset::EveryDay, bad, "", now, utc()).unwrap_err();
            assert!(said.contains("HH:MM"), "{bad:?}: {said}");
        }
        assert!(timing(Preset::Custom, "", "61 * * * *", now, utc()).is_err());
        assert!(timing(Preset::Custom, "", "", now, utc()).is_err());
        // The two hour chip has no time to get wrong.
        assert!(timing(Preset::EveryTwoHours, "junk", "", now, utc()).is_ok());
    }

    #[test]
    fn the_first_run_is_the_next_slot_after_now() {
        let now = at(8, 0);
        let weekdays = timing(Preset::Weekdays, "09:00", "", now, utc()).unwrap();
        assert_eq!(first_run(&weekdays, now, utc()), Some(at(9, 0)));
        // After nine on a Thursday the next weekday slot is Friday's.
        assert_eq!(
            first_run(&weekdays, at(9, 30), utc()),
            Some(at(9, 0) + 86_400)
        );
        let twice = timing(Preset::EveryTwoHours, "", "", now, utc()).unwrap();
        assert_eq!(first_run(&twice, now, utc()), Some(at(10, 0)));
        assert_eq!(
            first_run(&Timing::At(at(17, 0)), now, utc()),
            Some(at(17, 0))
        );
    }

    #[test]
    fn the_form_builds_the_write_the_daemon_takes() {
        let mut form = Form::new("~/app".to_string(), vec!["claude".into(), "codex".into()]);
        form.prompt = "  review the open PRs and post what needs me  ".to_string();
        let op = form
            .request(at(8, 0), utc(), Some("/Users/x"), &[])
            .unwrap();
        assert_eq!(
            op,
            ScheduleOp::Add {
                name: "review-the-open-prs".to_string(),
                cron: Some("0 9 * * 1-5".to_string()),
                at: None,
                watch: None,
                agent: Some("claude".to_string()),
                cwd: "/Users/x/app".to_string(),
                space: None,
                prompt: "review the open PRs and post what needs me".to_string(),
            }
        );
        form.preset = Preset::Once;
        form.agent = 1;
        form.folder = "/srv/app".to_string();
        let ScheduleOp::Add {
            cron,
            at: when,
            agent,
            cwd,
            ..
        } = form.request(at(8, 0), utc(), None, &[]).unwrap()
        else {
            panic!("an add");
        };
        assert_eq!((cron, when), (None, Some(at(9, 0))));
        assert_eq!(
            (agent.as_deref(), cwd.as_str()),
            (Some("codex"), "/srv/app")
        );
        // On change sends the command to check and no time at all.
        form.preset = Preset::OnChange;
        assert!(
            form.request(at(8, 0), utc(), None, &[])
                .unwrap_err()
                .contains("command to check")
        );
        form.field = Field::Time;
        for c in "gh issue list --json number".chars() {
            type_into(&mut form, c);
        }
        let ScheduleOp::Add {
            cron,
            at: when,
            watch,
            ..
        } = form.request(at(8, 0), utc(), None, &[]).unwrap()
        else {
            panic!("an add");
        };
        assert_eq!(
            (cron, when, watch.as_deref()),
            (None, None, Some("gh issue list --json number"))
        );
        assert_eq!(form.time, "09:00", "the command is not typed into the time");
    }

    #[test]
    fn the_form_refuses_what_the_daemon_would() {
        let mut form = Form::new("/tmp".to_string(), vec!["claude".into()]);
        assert!(
            form.request(at(8, 0), utc(), None, &[])
                .unwrap_err()
                .contains("what the agent")
        );
        form.prompt = "x".to_string();
        form.folder = "app".to_string();
        assert!(
            form.request(at(8, 0), utc(), None, &[])
                .unwrap_err()
                .contains("folder")
        );
        form.folder = "/tmp".to_string();
        form.time = "nine".to_string();
        assert!(
            form.request(at(8, 0), utc(), None, &[])
                .unwrap_err()
                .contains("HH:MM")
        );
        form.time = "09:00".to_string();
        assert!(form.request(at(8, 0), utc(), None, &[]).is_ok());
    }

    #[test]
    fn a_name_is_what_the_daemon_allows_and_never_one_already_taken() {
        let none = &[][..];
        assert_eq!(
            name_of("review the open PRs and post what needs me", none),
            "review-the-open-prs"
        );
        assert_eq!(name_of("Look at: the (inbox)!", none), "look-at-the-inbox");
        assert_eq!(name_of("12345", none), "run-12345");
        assert_eq!(name_of("!!!", none), "schedule");
        assert!(name_of(&"w".repeat(100), none).chars().count() <= 32);
        let taken = [
            "review-the-open-prs".to_string(),
            "review-the-open-prs-2".to_string(),
        ];
        assert_eq!(
            name_of("review the open PRs", &taken),
            "review-the-open-prs-3"
        );
        // Every name made is one the store takes.
        for prompt in ["a b", "x.y_z", "9 9 9", "ünï cödé", "  "] {
            let name = name_of(prompt, none);
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                    && !name.bytes().all(|b| b.is_ascii_digit())
                    && name.len() <= 40,
                "{prompt:?} -> {name:?}"
            );
        }
    }

    #[test]
    fn a_form_the_daemon_refuses_comes_back_with_its_words_and_the_reason() {
        let mut p = pane(Vec::new());
        let mut form = Form::new("/nowhere".into(), vec!["claude".into()]);
        form.prompt = "look at the inbox".to_string();
        p.form = Some(form);
        p.key(b"\r", 1);
        assert!(p.form.is_none(), "the form waits on the daemon");
        let sends = p.tick(Instant::now()).0;
        let id = sends[0].request_id().to_string();
        p.answered(&ControlResponse::Failed {
            id,
            reason: "no folder /nowhere".to_string(),
        });
        let form = p.form.as_ref().expect("the form is back");
        assert_eq!(form.prompt, "look at the inbox");
        assert_eq!(form.error.as_deref(), Some("no folder /nowhere"));
        assert!(
            painted(&p, 50, FORM_ROWS)
                .join("\n")
                .contains("no folder /nowhere")
        );
        // Saved and taken, it does not come back.
        p.key(b"\r", 1);
        let id = p.tick(Instant::now()).0[0].request_id().to_string();
        p.answered(&ControlResponse::ScheduleWritten { id, schedule: 3 });
        assert!(p.form.is_none() && p.saving.is_none());
    }

    #[test]
    fn the_agents_are_the_four_and_the_owners_own_that_run_one_with_qasss_first() {
        let launchers = vec![
            crate::config::Launcher {
                name: "fast".into(),
                command: "FOO=1 /usr/bin/claude --fast".into(),
            },
            crate::config::Launcher {
                name: "shell".into(),
                command: "bash".into(),
            },
        ];
        let all = agents(&launchers, "claude");
        assert_eq!(all, ["claude", "codex", "gemini", "opencode", "fast"]);
        assert_eq!(agents(&launchers, "fast")[0], "fast");
        assert_eq!(agents(&launchers, "codex")[0], "codex");
    }

    #[test]
    fn the_panel_says_next_and_when_then_how_the_last_run_went() {
        let mut ran = info(1, "morning review");
        ran.last_result = Some("started".to_string());
        ran.last_at = Some(at(7, 0));
        ran.last_terminal = Some(12);
        let mut skipped = info(2, "dependency audit");
        skipped.last_result = Some("skipped: Mac asleep".to_string());
        let mut paused = info(3, "release notes");
        paused.paused = true;
        let rows = painted(&pane(vec![ran, skipped, paused]), 60, 12);
        let text = rows.join("\n");
        assert!(
            rows[0].contains("schedules") && rows[0].contains("+ add"),
            "{text}"
        );
        assert!(
            rows[1].contains(&format!("{ACTIVE} morning review"))
                && rows[1].contains("09:00   weekdays 09:00"),
            "{text}"
        );
        assert!(
            rows[2].contains("last \u{2713} started 07:00 \u{2192} tab 12"),
            "{text}"
        );
        assert!(rows[4].contains("last skipped, Mac asleep"), "{text}");
        // A paused schedule is one faint line, with no run line under it.
        assert!(
            rows[5].contains(&format!("{PAUSED} release notes")) && rows[5].contains("paused"),
            "{text}"
        );
        assert!(!rows[6].contains("last"), "{text}");
        assert!(rows.iter().all(|r| width::width(r) == 60), "{text}");
    }

    #[test]
    fn a_watch_is_drawn_from_the_daemons_words_and_its_form_asks_for_the_command() {
        let mut watch = info(1, "app-errors");
        (watch.cron, watch.watch) = (None, Some("tail log | grep ERROR".to_string()));
        watch.when = "on change, every 5m".to_string();
        watch.next_at = Some(at(8, 5));
        let rows = painted(&pane(vec![watch]), 60, 8).join("\n");
        assert!(
            rows.contains("app-errors") && rows.contains("on change, every 5m"),
            "{rows}"
        );
        let mut open = pane(Vec::new());
        open.form = Some(Form {
            preset: Preset::OnChange,
            field: Field::Time,
            ..Form::new("~/app".to_string(), vec!["claude".into()])
        });
        let rows = painted(&open, 70, 14).join("\n");
        assert!(rows.contains("[on change]"), "{rows}");
        assert!(
            rows.contains("check") && rows.contains("a command, like gh issue list"),
            "{rows}"
        );
        assert!(
            rows.contains("checked every 5m; new lines start the agent"),
            "{rows}"
        );
    }

    #[test]
    fn a_schedule_that_never_ran_says_so() {
        let rows = painted(&pane(vec![info(1, "a")]), 50, 8).join("\n");
        assert!(rows.contains("last not run yet"), "{rows}");
    }

    #[test]
    fn a_narrow_panel_gives_up_the_timing_words_before_the_name() {
        let rows = painted(&pane(vec![info(1, "morning review")]), 34, 8);
        assert!(
            rows[1].contains("morning review") && rows[1].contains("09:00"),
            "{}",
            rows[1]
        );
        assert!(!rows[1].contains("weekdays"), "{}", rows[1]);
        assert!(rows.iter().all(|r| width::width(r) == 34));
    }

    #[test]
    fn the_pick_stays_in_view_when_the_list_is_longer_than_the_panel() {
        let list: Vec<_> = (1..=9).map(|i| info(i, &format!("job {i}"))).collect();
        let mut p = pane(list);
        p.focus = true;
        for _ in 0..8 {
            p.key(b"j", 1);
        }
        let rows = painted(&p, 50, 9);
        let text = rows.join("\n");
        assert!(text.contains(&format!("\u{25b8}{ACTIVE} job 9")), "{text}");
        assert!(!text.contains("job 1 "), "{text}");
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn an_empty_raised_panel_points_at_plus_and_an_unraised_one_is_not_shown() {
        let mut p = pane(Vec::new());
        assert!(!p.shown() && p.rows_wanted(80) == 0);
        p.raised = true;
        assert!(p.shown());
        let text = painted(&p, 40, 6).join("\n");
        assert!(
            text.contains("Nothing scheduled.") && text.contains("+ adds one"),
            "{text}"
        );
        // Letting go of the keyboard puts an empty panel away.
        p.focus = true;
        p.release();
        assert!(!p.shown());
    }

    #[test]
    fn the_keys_on_a_row_write_what_they_say() {
        let mut ran = info(7, "morning");
        ran.last_terminal = Some(12);
        let mut p = pane(vec![ran, info(8, "audit")]);
        p.focus = true;
        assert_eq!(p.key(b"\r", 1), Out::Jump(12));
        p.key(b" ", 1);
        p.key(b"r", 1);
        let (sends, _) = p.tick(Instant::now());
        let ops: Vec<_> = sends
            .iter()
            .map(|r| match r {
                ControlRequest::ScheduleWrite { action, .. } => (**action).clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            ops,
            [
                ScheduleOp::Pause { schedule: 7 },
                ScheduleOp::Run { schedule: 7 }
            ]
        );
        // On the second one, which has no run yet, enter says so and goes nowhere.
        p.key(b"j", 1);
        assert_eq!(p.key(b"\r", 1), Out::Stay);
        assert!(
            p.notice
                .as_deref()
                .is_some_and(|n| n.contains("has not run yet"))
        );
        // A paused one resumes.
        let mut paused = info(9, "old");
        paused.paused = true;
        let mut p = pane(vec![paused]);
        p.key(b" ", 1);
        assert!(matches!(
            &p.tick(Instant::now()).0[0],
            ControlRequest::ScheduleWrite { action, .. } if **action == ScheduleOp::Resume { schedule: 9 }
        ));
    }

    #[test]
    fn remove_asks_once_and_only_a_capital_y_removes() {
        let mut p = pane(vec![info(7, "morning")]);
        p.focus = true;
        p.key(b"x", 1);
        let text = painted(&p, 60, 8).join("\n");
        assert!(
            text.contains("Y removes morning, any other key keeps it"),
            "{text}"
        );
        // Anything else keeps it, and is spent on that.
        assert_eq!(p.key(b"x", 1), Out::Stay);
        assert!(p.tick(Instant::now()).0.is_empty());
        assert!(!painted(&p, 60, 8).join("\n").contains("Y removes"));
        p.key(b"x", 1);
        p.key(b"Y", 1);
        assert!(matches!(
            &p.tick(Instant::now()).0[0],
            ControlRequest::ScheduleWrite { action, .. } if **action == ScheduleOp::Remove { schedule: 7 }
        ));
    }

    #[test]
    fn the_prefix_a_mouse_report_and_a_detach_are_not_the_panels() {
        let mut p = pane(vec![info(1, "a")]);
        assert_eq!(p.key(b"\x01c", 1), Out::Pass(b"\x01c".to_vec()));
        assert_eq!(
            p.key(b"\x1b[<0;5;3M", 1),
            Out::Pass(b"\x1b[<0;5;3M".to_vec())
        );
        assert_eq!(p.key(b"\x11", 1), Out::Pass(b"\x11".to_vec()));
        assert_eq!(p.key(b"\x1b", 1), Out::Back);
        assert_eq!(p.key(b"+", 1), Out::Add);
    }

    #[test]
    fn the_daemons_answers_name_what_was_done_and_pick_what_was_added() {
        let mut p = pane(vec![info(1, "a")]);
        p.write(ScheduleOp::Pause { schedule: 1 }, "paused a".to_string());
        let id = format!("{WRITE_ID}-1");
        assert!(p.answered(&ControlResponse::ScheduleWritten { id, schedule: 1 }));
        assert_eq!(p.notice.as_deref(), Some("paused a"));
        assert!(p.stale);
        // The new one is picked once the list has it.
        p.write(ScheduleOp::Run { schedule: 0 }, "added b".to_string());
        let id = format!("{WRITE_ID}-2");
        p.answered(&ControlResponse::ScheduleWritten { id, schedule: 2 });
        p.answered(&ControlResponse::Schedules {
            id: READ_ID.to_string(),
            schedules: vec![info(1, "a"), info(2, "b")],
        });
        assert_eq!(p.picked().map(|s| s.id), Some(2));
        // A refusal is said as the daemon words it.
        p.write(ScheduleOp::Remove { schedule: 5 }, String::new());
        let id = format!("{WRITE_ID}-3");
        p.answered(&ControlResponse::Failed {
            id,
            reason: "no schedule 5".to_string(),
        });
        assert_eq!(p.notice.as_deref(), Some("no schedule 5"));
    }

    #[test]
    fn an_older_daemon_hides_the_panel() {
        let mut p = pane(vec![info(1, "a")]);
        assert!(p.shown());
        let said = "unknown variant `ScheduleRead`, expected one of".to_string();
        assert!(p.answered(&ControlResponse::Failed {
            id: String::new(),
            reason: said
        }));
        assert!(p.unsupported && !p.shown() && p.rows_wanted(80) == 0);
    }

    #[test]
    fn the_form_walks_its_fields_and_changes_its_chip_with_the_arrows() {
        let mut p = pane(Vec::new());
        p.form = Some(Form::new(
            "/tmp".into(),
            vec!["claude".into(), "codex".into()],
        ));
        let form = |p: &Pane| p.form.clone().unwrap();
        assert_eq!(
            (form(&p).field, form(&p).preset),
            (Field::When, Preset::Weekdays)
        );
        p.key(b"\x1b[C", 1);
        assert_eq!(form(&p).preset, Preset::EveryDay);
        p.key(b"\x1b[D", 1);
        p.key(b"\x1b[D", 1);
        assert_eq!(form(&p).preset, Preset::Once);
        p.key(b"\t", 1);
        assert_eq!(form(&p).field, Field::Time);
        p.key(b"\t", 1);
        p.key(b"\x1b[C", 1);
        assert_eq!((form(&p).field, form(&p).agent), (Field::Agent, 1));
        p.key(b"\x1b[Z", 1);
        assert_eq!(form(&p).field, Field::Time);
        // The two hour chip has no time to land on.
        p.key(b"\x1b[Z", 1);
        for _ in 0..3 {
            p.key(b"\x1b[C", 1);
        }
        assert_eq!(form(&p).preset, Preset::EveryTwoHours);
        p.key(b"\t", 1);
        assert_eq!(form(&p).field, Field::Agent);
    }

    #[test]
    fn typing_goes_into_the_field_and_the_time_takes_only_a_clock() {
        let mut p = pane(Vec::new());
        p.form = Some(Form::new("/tmp".into(), vec!["claude".into()]));
        p.key(b"\t", 1);
        p.key(b"\x15", 1);
        p.key(b"7:3x0", 1);
        assert_eq!(p.form.as_ref().unwrap().time, "7:30");
        p.key(b"\x7f", 1);
        assert_eq!(p.form.as_ref().unwrap().time, "7:3");
        for _ in 0..3 {
            p.key(b"\t", 1);
        }
        assert_eq!(p.form.as_ref().unwrap().field, Field::Prompt);
        p.key("check the PRs".as_bytes(), 1);
        assert_eq!(p.form.as_ref().unwrap().prompt, "check the PRs");
    }

    #[test]
    fn enter_saves_the_form_and_a_refusal_stays_on_it() {
        let mut p = pane(Vec::new());
        p.form = Some(Form::new("/tmp".into(), vec!["claude".into()]));
        p.key(b"\r", 1);
        let form = p.form.as_ref().expect("still open");
        assert!(
            form.error
                .as_deref()
                .unwrap_or("")
                .contains("what the agent")
        );
        assert!(
            painted(&p, 44, FORM_ROWS)
                .join("\n")
                .contains("write what the agent should do")
        );
        for _ in 0..4 {
            p.key(b"\t", 1);
        }
        p.key(b"look at the inbox", 1);
        p.key(b"\r", 1);
        assert!(p.form.is_none());
        let sends = p.tick(Instant::now()).0;
        assert!(matches!(
            &sends[0],
            ControlRequest::ScheduleWrite { action, .. }
                if matches!(&**action, ScheduleOp::Add { name, cron: Some(c), .. }
                    if name == "look-at-the-inbox" && c == "0 9 * * 1-5")
        ));
    }

    #[test]
    fn several_keys_in_one_read_are_each_taken() {
        let mut p = pane(vec![info(1, "a"), info(2, "b"), info(3, "c")]);
        p.focus = true;
        p.key(b"jj", 1);
        assert_eq!(p.picked().map(|s| s.id), Some(3));
        p.key(b"\x1b[A\x1b[A", 1);
        assert_eq!(p.picked().map(|s| s.id), Some(1));
        // In the form a tab and the words after it arrive together.
        p.form = Some(Form::new("/tmp".into(), vec!["claude".into()]));
        p.key(b"\t\x15\x7f08:15\t\t\t", 1);
        let form = p.form.as_ref().unwrap();
        assert_eq!((form.time.as_str(), form.field), ("08:15", Field::Prompt));
        p.key("ünï\x1b[D".as_bytes(), 1);
        assert_eq!(p.form.as_ref().unwrap().prompt, "ünï");
    }

    #[test]
    fn the_form_wants_exactly_the_rows_it_paints_at_every_width() {
        let mut p = pane(Vec::new());
        let mut form = Form::new("/tmp".into(), vec!["claude".into()]);
        form.prompt = "x".to_string();
        p.form = Some(form);
        // From the narrowest panel that is drawn beside the terminal; under it the chips take a
        // fourth row, and the form clips its last blank.
        for cols in 38..170 {
            let rows = p.rows_wanted(cols);
            assert!(
                (FORM_FIXED + 1..=FORM_ROWS).contains(&rows),
                "{cols}: {rows}"
            );
            let text = painted(&p, cols, rows);
            // Everything the form says is there, with no blank row left over.
            assert!(
                text.iter().any(|r| r.contains("first run")),
                "{cols}: {text:?}"
            );
            assert!(
                text.iter().any(|r| r.contains("enter save")),
                "{cols}: {text:?}"
            );
            assert!(text[rows - 3].contains("first run"), "{cols}: {text:?}");
        }
    }

    #[test]
    fn escape_puts_the_form_away_and_the_panel_stays() {
        let mut p = pane(vec![info(1, "a")]);
        p.form = Some(Form::new("/tmp".into(), vec!["claude".into()]));
        assert_eq!(p.key(b"\x1b", 1), Out::Stay);
        assert!(p.form.is_none() && p.shown());
    }

    #[test]
    fn the_form_reads_the_first_run_back_and_fits_a_narrow_panel() {
        let mut p = pane(Vec::new());
        let mut form = Form::new("/Users/x/app".into(), vec!["claude".into(), "codex".into()]);
        form.prompt = "review the open PRs".to_string();
        p.form = Some(form);
        let rows = painted(&p, 38, FORM_ROWS);
        let text = rows.join("\n");
        assert!(rows[0].contains("new schedule"), "{text}");
        assert!(
            text.contains("[once] [weekdays]") && text.contains("[custom]"),
            "{text}"
        );
        assert!(text.contains("time    09:00"), "{text}");
        assert!(text.contains("agent   claude"), "{text}");
        assert!(text.contains("folder  /Users/x/app"), "{text}");
        assert!(text.contains("prompt  review the open PRs"), "{text}");
        // 08:00 UTC on a Thursday: the weekdays chip fires at nine, today.
        assert!(text.contains("first run 09:00"), "{text}");
        assert!(rows.iter().all(|r| width::width(r) == 38), "{text}");
        assert_eq!(rows.len(), FORM_ROWS);
        // Wide, the chips take one row.
        let wide = painted(&p, 70, FORM_ROWS).join("\n");
        assert!(
            wide.contains("[once] [weekdays] [every day] [every 2h] [custom]"),
            "{wide}"
        );
    }

    #[test]
    fn a_click_lands_on_the_row_the_chip_or_the_plus_that_was_drawn() {
        let rect = crate::layout::Rect {
            x: 10,
            y: 3,
            cols: 50,
            rows: 9,
        };
        let mut p = pane(vec![info(1, "a"), info(2, "b")]);
        let v = view(&p, &ctx(), rect.cols, rect.rows);
        // The first schedule's name row, and the second's (its run line is the same row's).
        assert_eq!(hit_at(&v, rect, 3 + 1, 10 + 5), Some(Hit::Row(0)));
        assert_eq!(hit_at(&v, rect, 3 + 3, 10 + 5), Some(Hit::Row(1)));
        assert_eq!(hit_at(&v, rect, 3, 10 + rect.cols - 5), Some(Hit::Add));
        assert_eq!(hit_at(&v, rect, 3, 10 + 20), None);
        assert_eq!(p.click(Hit::Row(1)), Out::Stay);
        assert_eq!(p.picked().map(|s| s.id), Some(2));
        assert_eq!(p.click(Hit::Add), Out::Add);
        p.form = Some(Form::new("/tmp".into(), vec!["claude".into()]));
        let v = view(&p, &ctx(), 70, FORM_ROWS);
        let rect = crate::layout::Rect {
            x: 0,
            y: 0,
            cols: 70,
            rows: FORM_ROWS,
        };
        // The `[every 2h]` chip: after `[once] [weekdays] [every day] `.
        let x = 2 + LABEL + "[once] [weekdays] [every day] ".len() + 2;
        assert_eq!(hit_at(&v, rect, 1, x), Some(Hit::Chip(3)));
        p.click(Hit::Chip(3));
        assert_eq!(p.form.as_ref().unwrap().preset, Preset::EveryTwoHours);
    }
}
