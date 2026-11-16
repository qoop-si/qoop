//! d-schedules: the date strip and the selected day's runs, every schedule with its switch,
//! and the open run's report (header, its last message, its task, what it produced, earlier
//! runs), with Edit, Pause and Run now; New and Edit share one sheet.
//!
//! This Mac's schedules by default; another machine's are read through `Via`. Its runs come
//! from `ScheduleRuns`, asked again whenever the schedules change.

use std::collections::HashMap;

use gpui::{AnyElement, Context, Entity, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{ControlRequest, ScheduleInfo, ScheduleOp, ScheduleRun};
use qoop_tui::schedules::{Preset, Timing};

use crate::app::{Desk, Page};
use crate::field::{Field, FieldColors};
use crate::link::{self, Up};
use crate::state;
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

/// How long a run may go without a tab before the daemon counts it as never started
/// (`qoop_store::schedule::NO_TAB_FOR`, pinned by a test).
pub const NO_TAB_FOR: i64 = 120;

/// How many runs of each schedule are read.
const RUNS_READ: u32 = 50;

/// A run's folder read for its report: the read's id, the schedule, and the changes once they came
/// (or why not).
pub(crate) type Produced = (String, i64, Option<Result<crate::review::Changes, String>>);

/// The schedules page: which machine, its runs, the day and the run open, and the reads in
/// flight.
#[derive(Default)]
pub struct SchedulePage {
    /// A space of the machine shown; `None` is This Mac.
    pub via: Option<i64>,
    /// The schedules of the machine shown when it is not This Mac, and the read that asked.
    pub remote: Vec<ScheduleInfo>,
    pub(crate) read: Option<String>,
    /// Runs by machine (`state::machine_of`) and schedule, newest first.
    pub runs: HashMap<(usize, i64), Vec<ScheduleRun>>,
    pub(crate) asked: HashMap<String, (usize, i64)>,
    pub day: Option<i64>,
    /// The run whose report is open: (schedule, run).
    pub open: Option<(i64, i64)>,
    pub calendar: bool,
    /// The month the calendar shows, as days since 1970 of its first day.
    pub(crate) month: Option<i64>,
    /// The report's task read, and the folder's changes read now, each under its id.
    pub(crate) task: Option<(String, Option<state::Strip>)>,
    pub(crate) produced: Option<Produced>,
    pub sheet: Option<ScheduleSheet>,
    /// Pause, Resume and Run now in flight, by id: the schedule and what was asked.
    pub(crate) writes: HashMap<String, (i64, Write)>,
    /// What the last of those came to, per schedule, as a sentence.
    pub said: HashMap<i64, String>,
}

/// What a button asked of a schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    Pause,
    Resume,
    Run,
}

/// A run of `schedule` among `runs` is open (claimed, or started and not ended), whichever
/// it is: Run now waits for it, as the daemon does. A run that has had no tab for
/// `qoop_store::schedule::NO_TAB_FOR` seconds is one that never started, as the daemon
/// counts it, and holds nothing back.
pub fn running_now(runs: &[ScheduleRun], schedule: i64, now: i64) -> bool {
    runs.iter().any(|r| {
        r.schedule == schedule
            && r.ended_at.is_none()
            && matches!(r.result.as_str(), "claimed" | "started")
            && (r.terminal.is_some() || now - r.started_at < NO_TAB_FOR)
    })
}

/// A refusal of a schedule write as a sentence.
pub fn write_refusal(reason: &str, machine: &str) -> String {
    match reason {
        "still_running" => "Its last run is still working. Run now is back when it ends.".into(),
        r if r.contains("unknown variant") || r.contains("unknown field") => {
            format!("qoop on {machine} is too old for this. Update it there first.")
        }
        other => other.to_string(),
    }
}

/// New schedule and Edit: one sheet.
pub struct ScheduleSheet {
    /// The schedule edited; `None` for a new one.
    pub editing: Option<i64>,
    pub name: Entity<Field>,
    pub time: Entity<Field>,
    pub cron: Entity<Field>,
    pub folder: Entity<Field>,
    pub prompt: Entity<Field>,
    pub preset: Preset,
    /// The agent by name: a schedule's own stays its own even when this Mac does not offer it.
    pub agent: String,
    pub space: Option<String>,
    /// The schedule as it was when Edit opened, and the timing the sheet showed for it: only
    /// what the person changed is sent back.
    pub(crate) original: Option<ScheduleInfo>,
    pub(crate) first_timing: (Preset, String, String),
    pub(crate) id: Option<String>,
    pub refused: Option<String>,
}

impl Desk {
    /// The machine shown's number: 0 for This Mac.
    fn sched_machine(&self) -> usize {
        self.schedule_page.via.map_or(0, state::machine_of)
    }

    /// The schedules of the machine shown.
    pub(crate) fn shown_schedules(&self) -> &[ScheduleInfo] {
        match self.schedule_page.via {
            None => &self.model.schedules,
            Some(_) => &self.schedule_page.remote,
        }
    }

    /// A run's mark and words. One still going reads as its tab does now (Needs you while its
    /// agent asks, neutral while its machine is not reached), not a bare Working.
    pub(crate) fn run_words(&self, run: &ScheduleRun) -> (Option<state::Kind>, String) {
        let (kind, words) = state::run_words(run);
        let tab = run
            .terminal
            .and_then(|t| u32::try_from(t).ok())
            .and_then(|t| self.model.row(t));
        match (kind, tab) {
            (Some(state::Kind::Status(qoop_domain::status::Status::Working)), Some(row)) => (
                Some(self.shown_kind(state::Entry::Live(row))),
                self.shown_word(row),
            ),
            _ => (kind, words),
        }
    }

    fn shown_runs(&self) -> Vec<ScheduleRun> {
        let machine = self.sched_machine();
        self.schedule_page
            .runs
            .iter()
            .filter(|((m, _), _)| *m == machine)
            .flat_map(|(_, runs)| runs.iter().cloned())
            .collect()
    }

    /// This Mac's runs, for home's "Last:" line.
    pub(crate) fn local_runs(&self) -> Vec<ScheduleRun> {
        self.schedule_page
            .runs
            .iter()
            .filter(|((m, _), _)| *m == 0)
            .flat_map(|(_, runs)| runs.iter().cloned())
            .collect()
    }

    /// A request to the machine shown: through `Via` for another machine.
    fn sched_ask(&self, request: ControlRequest) {
        match self.schedule_page.via {
            None => self.ask(request),
            Some(workspace) => self.ask(ControlRequest::Via {
                id: link::fresh_id("via"),
                workspace,
                request: Box::new(request),
            }),
        }
    }

    /// Read the runs of every schedule of the machine shown: what the page shows while it is
    /// open, else only the newest (home's "Last:" line).
    pub(crate) fn ask_runs(&mut self, schedules: &[i64]) {
        let machine = self.sched_machine();
        let limit = if self.page == Page::Schedules {
            RUNS_READ
        } else {
            1
        };
        for &schedule in schedules {
            let id = link::fresh_id("runs");
            self.schedule_page
                .asked
                .insert(id.clone(), (machine, schedule));
            self.sched_ask(ControlRequest::ScheduleRuns {
                id,
                schedule,
                before: None,
                limit,
            });
        }
    }

    /// Another machine's schedules read again (its runs follow), keeping the day and the run
    /// open: a change came.
    pub(crate) fn refresh_remote(&mut self) {
        if self.schedule_page.via.is_none() || self.page != Page::Schedules {
            return;
        }
        let id = link::fresh_id("schedules");
        self.schedule_page.read = Some(id.clone());
        self.sched_ask(ControlRequest::ScheduleRead { id });
    }

    /// Show another machine's schedules (`Some` space of it), or This Mac's.
    fn show_machine(&mut self, via: Option<i64>, cx: &mut Context<Self>) {
        let page = &mut self.schedule_page;
        page.via = via;
        page.remote.clear();
        page.open = None;
        page.day = None;
        page.read = None;
        match via {
            None => {
                let ids: Vec<i64> = self.model.schedules.iter().map(|s| s.id).collect();
                self.ask_runs(&ids);
            }
            Some(_) => {
                let id = link::fresh_id("schedules");
                self.schedule_page.read = Some(id.clone());
                self.sched_ask(ControlRequest::ScheduleRead { id });
            }
        }
        cx.notify();
    }

    /// A `Schedules` answer: the machine shown's, else this Mac's; their runs are read again.
    pub(crate) fn schedules_came(&mut self, id: &str, schedules: Vec<ScheduleInfo>) {
        let ids: Vec<i64> = schedules.iter().map(|s| s.id).collect();
        if self.schedule_page.read.as_deref() == Some(id) {
            self.schedule_page.remote = schedules;
            self.ask_runs(&ids);
            return;
        }
        self.model.schedules = schedules;
        if self.schedule_page.via.is_none() {
            self.ask_runs(&ids);
        } else {
            // Home's "Last:" line keeps reading this Mac's newest run.
            for &schedule in &ids {
                let id = link::fresh_id("runs");
                self.schedule_page.asked.insert(id.clone(), (0, schedule));
                self.ask(ControlRequest::ScheduleRuns {
                    id,
                    schedule,
                    before: None,
                    limit: 1,
                });
            }
        }
    }

    /// A `ScheduleRuns` answer: whether it was one of the page's.
    pub(crate) fn runs_came(&mut self, id: &str, runs: Vec<ScheduleRun>) -> bool {
        match self.schedule_page.asked.remove(id) {
            Some(key) => {
                self.schedule_page.runs.insert(key, runs);
                true
            }
            None => false,
        }
    }

    /// The report's task (`Tasks`) or the folder's changes (`Changes`) came: whether it was
    /// one of the report's.
    pub(crate) fn report_came(&mut self, reply: &qoop_runtime::control::ControlResponse) -> bool {
        use qoop_runtime::control::ControlResponse as R;
        let id = reply.request_id().to_string();
        let machine = self.machine_shown_name();
        let page = &mut self.schedule_page;
        match reply {
            R::Tasks {
                tasks,
                events,
                item,
                ..
            } if page.task.as_ref().is_some_and(|(i, _)| *i == id) => {
                let strip = state::Strip::of(tasks.clone(), events.clone(), item.clone());
                page.task = Some((id, Some(strip)));
                true
            }
            R::Changes {
                root,
                base,
                files,
                diff,
                cut,
                ..
            } if page.produced.as_ref().is_some_and(|(i, ..)| *i == id) => {
                if let Some((_, _, read)) = page.produced.as_mut() {
                    *read = Some(Ok(crate::review::Changes {
                        root: root.clone(),
                        base: base.clone(),
                        files: files.clone(),
                        diffs: crate::review::split_diff(diff),
                        cut: *cut,
                    }));
                }
                true
            }
            R::Failed { reason, .. } if page.produced.as_ref().is_some_and(|(i, ..)| *i == id) => {
                if let Some((_, _, read)) = page.produced.as_mut() {
                    *read = Some(Err(reason.clone()));
                }
                true
            }
            R::Failed { .. } if page.asked.remove(&id).is_some() => true,
            R::Failed { reason, .. } if page.writes.contains_key(&id) => {
                if let Some((schedule, _)) = page.writes.remove(&id) {
                    page.said.insert(schedule, write_refusal(reason, &machine));
                }
                true
            }
            R::ScheduleWritten { .. } if page.writes.contains_key(&id) => {
                if let Some((schedule, write)) = page.writes.remove(&id) {
                    match write {
                        Write::Run => page.said.insert(schedule, "A run started.".into()),
                        _ => page.said.remove(&schedule),
                    };
                }
                true
            }
            R::Failed { reason, .. }
                if page
                    .sheet
                    .as_ref()
                    .is_some_and(|s| s.id.as_deref() == Some(id.as_str())) =>
            {
                if let Some(sheet) = page.sheet.as_mut() {
                    sheet.refused = Some(write_refusal(reason, &machine));
                }
                true
            }
            R::ScheduleWritten { .. }
                if page
                    .sheet
                    .as_ref()
                    .is_some_and(|s| s.id.as_deref() == Some(id.as_str())) =>
            {
                page.sheet = None;
                true
            }
            _ => false,
        }
    }

    /// Open a run's report: its task read, and for the newest run of a schedule on this Mac
    /// what is in its folder now.
    fn open_run(&mut self, schedule: i64, run: &ScheduleRun, cx: &mut Context<Self>) {
        self.schedule_page.open = Some((schedule, run.id));
        self.schedule_page.task = None;
        self.schedule_page.produced = None;
        let here = self.schedule_page.via.is_none();
        if let (Some(task), true) = (run.task, here) {
            let id = link::fresh_id("run-task");
            self.schedule_page.task = Some((id.clone(), None));
            self.ask(ControlRequest::TaskRead {
                id,
                board: false,
                task: Some(task),
                terminal: None,
                token: None,
                before: None,
            });
        }
        let newest = self
            .schedule_page
            .runs
            .get(&(0, schedule))
            .and_then(|runs| runs.iter().max_by_key(|r| r.started_at))
            .is_some_and(|r| r.id == run.id);
        let cwd = self
            .model
            .schedules
            .iter()
            .find(|s| s.id == schedule)
            .map(|s| s.cwd.clone());
        if let (true, true, Some(cwd)) = (here, newest, cwd) {
            let id = link::fresh_id("produced");
            self.schedule_page.produced = Some((id.clone(), self.now, None));
            let terminal = run
                .terminal
                .and_then(|t| u32::try_from(t).ok())
                .unwrap_or(0);
            let _ = self.up.send(Up::Changes { id, terminal, cwd });
        }
        cx.notify();
    }

    /// The machine shown, as its tile names it.
    fn machine_shown_name(&self) -> String {
        self.schedule_page
            .via
            .and_then(|w| self.model.space(w))
            .map_or_else(|| "This Mac".to_string(), state::machine_name)
    }

    /// Pause, Resume or Run now: sent, tracked, and its outcome said on the page.
    fn press(&mut self, schedule: i64, write: Write, cx: &mut Context<Self>) {
        let pending = |w: &Write| *w == Write::Run;
        if write == Write::Run
            && self
                .schedule_page
                .writes
                .values()
                .any(|(s, w)| *s == schedule && pending(w))
        {
            return;
        }
        let action = match write {
            Write::Pause => ScheduleOp::Pause { schedule },
            Write::Resume => ScheduleOp::Resume { schedule },
            Write::Run => ScheduleOp::Run { schedule },
        };
        let id = self.write_schedule(action);
        self.schedule_page.writes.insert(id, (schedule, write));
        self.schedule_page.said.remove(&schedule);
        cx.notify();
    }

    /// Run now can be pressed: no Run now in flight, and the newest run has ended.
    pub(crate) fn can_run(&self, schedule: i64, runs: &[ScheduleRun]) -> bool {
        !running_now(runs, schedule, self.now)
            && !self
                .schedule_page
                .writes
                .values()
                .any(|(s, w)| *s == schedule && *w == Write::Run)
    }

    fn write_schedule(&mut self, action: ScheduleOp) -> String {
        let id = link::fresh_id("schedule");
        self.sched_ask(ControlRequest::ScheduleWrite {
            id: id.clone(),
            terminal: None,
            token: None,
            action: Box::new(action),
        });
        id
    }

    fn field(&self, placeholder: &str, text: &str, cx: &mut Context<Self>) -> Entity<Field> {
        let t = self.theme();
        let colors = FieldColors {
            text: c(t.ink).into(),
            placeholder: c(t.t2).into(),
            caret: c(t.ink).into(),
            selection: ca(t.fill2).into(),
        };
        let field = cx.new(|cx| {
            let mut f = Field::new(placeholder, colors, cx);
            f.text = text.to_string();
            f
        });
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        field
    }

    /// New schedule, or Edit of `editing`: one sheet, filled from the schedule when editing.
    fn open_sheet(
        &mut self,
        editing: Option<&ScheduleInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (preset, time, cron) = match editing {
            Some(s) if s.watch.is_some() => (
                Preset::OnChange,
                String::new(),
                s.watch.clone().unwrap_or_default(),
            ),
            Some(s) if s.at.is_some() => (
                Preset::Once,
                s.at.map(|at| state::clock(at, self.zone))
                    .unwrap_or_default(),
                String::new(),
            ),
            Some(s) => (
                Preset::Custom,
                String::new(),
                s.cron.clone().unwrap_or_default(),
            ),
            None => (Preset::Weekdays, "09:00".to_string(), String::new()),
        };
        let agent = editing.map_or_else(
            || {
                self.offers
                    .first()
                    .map_or_else(|| "claude".to_string(), |(name, _)| name.clone())
            },
            |s| s.agent.clone(),
        );
        let first_timing = (preset, time.clone(), cron.clone());
        let name = self.field("nightly-tests", editing.map_or("", |s| s.name.as_str()), cx);
        let time = self.field("09:00", &time, cx);
        let cron = self.field("0 2 * * 1-5, or the command to check", &cron, cx);
        let folder = self.field(
            "~/code/acme-web",
            editing.map_or("", |s| s.cwd.as_str()),
            cx,
        );
        let prompt = self.field(
            "What the agent does each run",
            editing.map_or("", |s| s.prompt.as_str()),
            cx,
        );
        let focus = name.read(cx).focus.clone();
        window.focus(&focus, cx);
        self.schedule_page.sheet = Some(ScheduleSheet {
            editing: editing.map(|s| s.id),
            name,
            time,
            cron,
            folder,
            prompt,
            preset,
            agent,
            space: editing.and_then(|s| s.space.clone()),
            original: editing.cloned(),
            first_timing,
            id: None,
            refused: None,
        });
        cx.notify();
    }

    /// The sheet's timing as the daemon takes it, or why it cannot be.
    fn sheet_timing(&self, sheet: &ScheduleSheet, cx: &Context<Self>) -> Result<Timing, String> {
        qoop_tui::schedules::timing(
            sheet.preset,
            &sheet.time.read(cx).text,
            &sheet.cron.read(cx).text,
            self.now,
            self.zone,
        )
    }

    fn save_sheet(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.schedule_page.sheet.as_ref() else {
            return;
        };
        let text = |f: &Entity<Field>| f.read(cx).text.trim().to_string();
        let (name, folder, prompt) = (text(&sheet.name), text(&sheet.folder), text(&sheet.prompt));
        let problem = match (name.is_empty(), folder.is_empty(), prompt.is_empty()) {
            (true, ..) => Some("Give it a name.".to_string()),
            (_, true, _) => Some("Say the folder it runs in.".to_string()),
            (.., true) => Some("Say what the agent does each run.".to_string()),
            _ => None,
        };
        // An edit sends only what changed: an unchanged timing keeps a one-off's date and the
        // schedule's last slot, an unchanged agent stays its own.
        let shown = (sheet.preset, text(&sheet.time), text(&sheet.cron));
        let (first_preset, first_time, first_cron) = &sheet.first_timing;
        let timing_changed = sheet.original.is_none()
            || shown
                != (
                    *first_preset,
                    first_time.trim().to_string(),
                    first_cron.trim().to_string(),
                );
        let timing = self.sheet_timing(sheet, cx);
        let original = sheet.original.clone();
        let agent = Some(sheet.agent.clone());
        let (space, editing) = (sheet.space.clone(), sheet.editing);
        let refused = match (problem, &timing) {
            (Some(p), _) => Some(p),
            (None, Err(why)) => Some(format!("When: {why}")),
            _ => None,
        };
        if let Some(why) = refused {
            if let Some(sheet) = self.schedule_page.sheet.as_mut() {
                sheet.refused = Some(why);
            }
            cx.notify();
            return;
        }
        let Ok(timing) = timing else { return };
        let (cron, at, watch) = match timing {
            _ if !timing_changed => (None, None, None),
            Timing::Cron(c) => (Some(c), None, None),
            Timing::At(at) => (None, Some(at), None),
            Timing::Watch(w) => (None, None, Some(w)),
        };
        let changed = |now: String, was: Option<&String>| (Some(&now) != was).then_some(now);
        let action = match (editing, original) {
            (Some(schedule), Some(was)) => {
                let edit = ScheduleOp::Change {
                    schedule,
                    name: changed(name, Some(&was.name)),
                    cron,
                    at,
                    watch,
                    agent: agent.and_then(|a| changed(a, Some(&was.agent))),
                    cwd: changed(folder, Some(&was.cwd)),
                    space: None,
                    prompt: changed(prompt, Some(&was.prompt)),
                };
                if edit
                    == (ScheduleOp::Change {
                        schedule,
                        name: None,
                        cron: None,
                        at: None,
                        watch: None,
                        agent: None,
                        cwd: None,
                        space: None,
                        prompt: None,
                    })
                {
                    // Nothing changed: nothing to send.
                    self.schedule_page.sheet = None;
                    cx.notify();
                    return;
                }
                edit
            }
            _ => ScheduleOp::Add {
                name,
                cron,
                at,
                watch,
                agent,
                cwd: folder,
                space,
                prompt,
            },
        };
        let id = self.write_schedule(action);
        if let Some(sheet) = self.schedule_page.sheet.as_mut() {
            sheet.id = Some(id);
            sheet.refused = None;
        }
        cx.notify();
    }

    /// Escape on this page: the sheet, else the calendar. Whether it was taken.
    pub(crate) fn schedules_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let page = &mut self.schedule_page;
        let taken = if page.sheet.take().is_some() {
            true
        } else {
            std::mem::take(&mut page.calendar)
        };
        if taken {
            cx.notify();
        }
        taken
    }

    pub(crate) fn schedules_page(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let schedules = self.shown_schedules();
        let runs = self.shown_runs();
        let today = state::day_of(self.now, self.zone);
        // Another machine runs its crons on its own clock: only the next run it names itself.
        let coming = match self.schedule_page.via {
            None => state::coming_slots(schedules, self.now, self.now + 7 * 86_400, self.zone),
            Some(_) => {
                let mut next: Vec<(i64, i64)> = schedules
                    .iter()
                    .filter(|s| !s.paused)
                    .filter_map(|s| Some((s.next_at?, s.id)))
                    .collect();
                next.sort_unstable();
                next
            }
        };
        let strip = state::strip_days(&runs, &coming, today, self.zone, (6, 3));
        let day = self.schedule_page.day.unwrap_or(today);
        if schedules.is_empty() {
            return div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(10.))
                .p(px(24.))
                .child(self.machine_row(t, cx))
                .child(
                    div()
                        .text_size(px(13.5))
                        .text_color(c(t.t2))
                        .child("No schedules yet."),
                )
                .child(self.new_button(t, cx))
                .when(self.schedule_page.sheet.is_some(), |d| {
                    d.child(self.schedule_sheet(t, cx))
                })
                .into_any_element();
        }
        div()
            .size_full()
            .flex()
            .gap(px(12.))
            .child(
                div()
                    .id("schedules-left")
                    .flex_none()
                    .w(px(440.))
                    .h_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(self.machine_row(t, cx))
                            .child(div().flex_1())
                            .child(
                                ui::pill("calendar", "Calendar", t)
                                    .aria_expanded(self.schedule_page.calendar)
                                    .child(ui::icon(Icon::Clock, 14., c(t.t1)))
                                    .child("Calendar")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.schedule_page.calendar = !this.schedule_page.calendar;
                                        cx.notify();
                                    })),
                            )
                            .child(self.new_button(t, cx)),
                    )
                    .child(self.day_strip(&strip, day, today, t, cx))
                    .when(self.schedule_page.calendar, |d| {
                        d.child(self.calendar(&runs, &coming, today, t, cx))
                    })
                    .child(self.day_runs(day, today, &runs, &coming, t, cx))
                    .child(self.all_schedules(t, cx)),
            )
            .child(
                div()
                    .id("report")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(self.report(&runs, t, cx)),
            )
            .when(self.schedule_page.sheet.is_some(), |d| {
                d.child(self.schedule_sheet(t, cx))
            })
            .into_any_element()
    }

    fn new_button(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        ui::pill("new-schedule", "New schedule", t)
            .child(ui::icon(Icon::Plus, 14., c(t.t1)))
            .child("New schedule")
            .on_click(cx.listener(|this, _, window, cx| this.open_sheet(None, window, cx)))
    }

    /// This Mac and every other machine with a space: which one's schedules are shown.
    fn machine_row(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let machines = state::machines(&self.model.spaces);
        let shown = self.sched_machine();
        div()
            .id("machines")
            .role(Role::RadioGroup)
            .aria_label("Machine")
            .flex()
            .gap(px(4.))
            .when(machines.len() > 1, |d| {
                d.children(machines.into_iter().enumerate().map(|(n, (m, name, via))| {
                    let on = m == shown;
                    ui::chip(("machine", n as u64), &name, None, None, on, t)
                        .on_click(cx.listener(move |this, _, _, cx| this.show_machine(via, cx)))
                }))
            })
    }

    fn day_strip(
        &self,
        strip: &[state::StripDay],
        day: i64,
        today: i64,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("day-strip")
            .role(Role::RadioGroup)
            .aria_label("Days")
            .flex()
            .gap(px(6.))
            .children(strip.iter().map(|d| {
                let this_day = d.day;
                let on = this_day == day;
                let count = match (d.runs, d.coming) {
                    (0, 0) => String::new(),
                    (0, n) => format!("{n} next"),
                    (n, _) => format!("{n} runs"),
                };
                let label = format!(
                    "{}{}, {}",
                    state::day_label(this_day),
                    if this_day == today { ", today" } else { "" },
                    if count.is_empty() {
                        "nothing"
                    } else {
                        count.as_str()
                    }
                );
                ui::control(("day", this_day as u64), Role::RadioButton, label, t)
                    .aria_toggled(ui::toggled(on))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_w(px(58.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(10.))
                    .bg(c(if on { t.card2 } else { t.card }))
                    .when(this_day == today, |s| s.border_color(ca(t.line)))
                    .text_size(px(12.5))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(state::day_label(this_day)),
                    )
                    .when(this_day == today, |s| {
                        s.child(div().text_size(px(11.)).text_color(c(t.t2)).child("today"))
                    })
                    .child(div().text_size(px(11.)).text_color(c(t.t2)).child(count))
                    .when(d.failed > 0, |s| {
                        s.child(
                            div()
                                .text_size(px(11.))
                                .text_color(c(t.fail_text))
                                .child(format!("{} failed", d.failed)),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.schedule_page.day = Some(this_day);
                        cx.notify();
                    }))
            }))
    }

    /// The month of the selected day: each day with its runs, today a neutral ring and the
    /// word. Escape closes it.
    fn calendar(
        &self,
        runs: &[ScheduleRun],
        coming: &[(i64, i64)],
        today: i64,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let day = self.schedule_page.day.unwrap_or(today);
        let wall = qoop_runtime::schedule::Naive::from_secs(day * 86_400);
        let first = self
            .schedule_page
            .month
            .unwrap_or(day - i64::from(wall.day) + 1);
        let first_wall = qoop_runtime::schedule::Naive::from_secs(first * 86_400);
        let length = (28..=31)
            .rev()
            .find(|n| {
                let last = qoop_runtime::schedule::Naive::from_secs((first + n - 1) * 86_400);
                last.month == first_wall.month
            })
            .unwrap_or(28);
        let lead = (first + 4).rem_euclid(7);
        let count = |d: i64| {
            runs.iter()
                .filter(|r| state::day_of(r.started_at, self.zone) == d)
                .count()
                + coming
                    .iter()
                    .filter(|(at, _)| state::day_of(*at, self.zone) == d)
                    .count()
        };
        div()
            .id("calendar-pop")
            .role(Role::Dialog)
            .aria_label("Calendar")
            .p(px(12.))
            .rounded(px(14.))
            .bg(c(t.pop))
            .border_1()
            .border_color(ca(t.line))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        ui::link_button("month-back", "Earlier month", t).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let back =
                                    qoop_runtime::schedule::Naive::from_secs((first - 1) * 86_400);
                                this.schedule_page.month = Some(first - i64::from(back.day));
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                state::day_long(first)
                                    .split(' ')
                                    .skip(2)
                                    .collect::<String>(),
                            ),
                    )
                    .child(
                        ui::link_button("month-next", "Next month", t).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.schedule_page.month = Some(first + length);
                                cx.notify();
                            },
                        )),
                    )
                    .child(ui::kbd("esc", t)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .w(px(7. * 52.))
                    .children((0..lead).map(|_| div().w(px(52.)).h(px(40.))))
                    .children((0..length).map(|n| {
                        let d = first + n;
                        let runs_here = count(d);
                        ui::control(
                            ("cal-day", d as u64),
                            Role::Button,
                            format!(
                                "{}{}, {runs_here} runs",
                                state::day_label(d),
                                if d == today { ", today" } else { "" }
                            ),
                            t,
                        )
                        .w(px(52.))
                        .h(px(40.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .text_size(px(12.5))
                        .when(d == today, |s| s.border_color(ca(t.line)))
                        .hover(|s| s.bg(ca(t.fill)))
                        .child((n + 1).to_string())
                        .when(d == today, |s| {
                            s.child(div().text_size(px(10.)).text_color(c(t.t2)).child("today"))
                        })
                        .when(runs_here > 0 && d != today, |s| {
                            s.child(div().size(px(4.)).rounded_full().bg(c(t.t2)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.schedule_page.day = Some(d);
                            this.schedule_page.calendar = false;
                            cx.notify();
                        }))
                    })),
            )
    }

    /// The day's runs in the order they started; a coming day's slots as "Runs at 18:00".
    fn day_runs(
        &self,
        day: i64,
        today: i64,
        runs: &[ScheduleRun],
        coming: &[(i64, i64)],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let schedules = self.shown_schedules();
        let name = |id: i64| {
            schedules
                .iter()
                .find(|s| s.id == id)
                .map_or_else(String::new, |s| s.name.clone())
        };
        let mut here: Vec<&ScheduleRun> = runs
            .iter()
            .filter(|r| state::day_of(r.started_at, self.zone) == day)
            .collect();
        here.sort_by_key(|r| r.started_at);
        let failed = here.iter().filter(|r| state::run_failed(r)).count();
        let next: Vec<&(i64, i64)> = coming
            .iter()
            .filter(|(at, _)| state::day_of(*at, self.zone) == day)
            .collect();
        let open = self.schedule_page.open;
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .pt(px(6.))
                    .text_size(px(13.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(state::day_long(day)),
                    )
                    .child(div().text_color(c(t.t2)).child(match (here.len(), failed) {
                        (0, _) if day >= today => format!("{} next", next.len()),
                        (n, 0) => format!("{n} runs"),
                        (n, f) => format!("{n} runs, {f} failed"),
                    })),
            )
            .children(here.into_iter().take(60).map(|r| {
                let (kind, words) = self.run_words(r);
                let schedule = r.schedule;
                let run = r.clone();
                let on = open == Some((schedule, r.id));
                let failed = state::run_failed(r);
                ui::control(
                    ("run", r.id as u64),
                    Role::Button,
                    format!(
                        "{}, {}, {words}",
                        name(schedule),
                        state::clock(r.started_at, self.zone)
                    ),
                    t,
                )
                .flex()
                .gap(px(10.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(10.))
                .when(on, |d| d.bg(ca(t.fill)))
                .hover(|s| s.bg(ca(t.fill)))
                .child(
                    div()
                        .w(px(44.))
                        .flex_none()
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .child(state::clock(r.started_at, self.zone)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_size(px(12.5))
                                .child(name(schedule)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .text_size(px(12.))
                                .when_some(kind, |d, k| d.child(ui::mark(k, t, 9.)))
                                .child(
                                    div()
                                        .text_color(c(match kind {
                                            Some(k) => ui::word_ink(k, t),
                                            None => t.t2,
                                        }))
                                        .when(failed, |d| d.text_color(c(t.fail_text)))
                                        .child(words),
                                )
                                .when_some(r.terminal, |d, tab| {
                                    d.child(div().text_color(c(t.t2)).child(format!(
                                        "tab {}",
                                        qoop_tui::mux::daemon_terminal(tab as u32)
                                    )))
                                })
                                .when_some(state::took(r), |d, took| {
                                    d.child(div().text_color(c(t.t2)).child(took))
                                }),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_run(schedule, &run, cx)))
            }))
            .children(next.into_iter().map(|&(at, id)| {
                div()
                    .flex()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(5.))
                    .text_size(px(12.5))
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .font_family(MONO_FONT)
                            .text_color(c(t.t2))
                            .child(state::clock(at, self.zone)),
                    )
                    .child(div().flex_1().text_color(c(t.t2)).child(format!(
                        "{} runs at {}",
                        name(id),
                        state::clock(at, self.zone)
                    )))
            }))
    }

    /// Every schedule with its switch: off pauses it, on resumes it, at once.
    fn all_schedules(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let schedules = self.shown_schedules();
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .pt(px(10.))
                    .text_size(px(13.))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("All schedules"),
                    )
                    .child(div().text_color(c(t.t2)).child(schedules.len().to_string())),
            )
            .children(schedules.iter().map(|s| {
                let schedule = s.id;
                let on = !s.paused;
                let next = match (s.paused, s.next_at) {
                    (true, _) => "paused".to_string(),
                    (false, Some(at)) => state::clock_at(at, self.now, self.zone),
                    (false, None) => String::new(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .font_family(MONO_FONT)
                                            .text_size(px(12.5))
                                            .child(s.name.clone()),
                                    )
                                    .when_some(s.space.clone(), |d, space| {
                                        d.child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(c(t.t2))
                                                .child(space),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .child(format!("{}, {}", s.when, s.agent)),
                            ),
                    )
                    .child(div().text_size(px(11.5)).text_color(c(t.t2)).child(next))
                    .child(
                        ui::switch(("schedule-on", schedule as u64), &s.name, on, t)
                            .debug_selector(move || format!("schedule-on-{schedule}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let write = if on { Write::Pause } else { Write::Resume };
                                this.press(schedule, write, cx);
                            })),
                    )
            }))
            .child(
                div()
                    .px(px(8.))
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child("A switch pauses or resumes at once."),
            )
    }

    /// The open run's report: header, Summary, Task, Produced (newest run on this Mac only),
    /// Earlier runs, then Edit, Pause and Run now.
    fn report(&self, runs: &[ScheduleRun], t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some((schedule, run_id)) = self.schedule_page.open else {
            return div()
                .p(px(24.))
                .text_size(px(13.))
                .text_color(c(t.t2))
                .child("Pick a run to read its report.")
                .into_any_element();
        };
        let Some(s) = self.shown_schedules().iter().find(|s| s.id == schedule) else {
            return div().into_any_element();
        };
        let Some(run) = runs.iter().find(|r| r.id == run_id) else {
            return div().into_any_element();
        };
        let (kind, words) = self.run_words(run);
        let tab = run
            .terminal
            .and_then(|t| u32::try_from(t).ok())
            .filter(|t| self.model.row(*t).is_some());
        let space = s.space.clone().unwrap_or_default();
        let range = match run.ended_at {
            Some(end) => format!(
                "{}, {} to {}",
                state::day_long(state::day_of(run.started_at, self.zone)),
                state::clock(run.started_at, self.zone),
                state::clock(end, self.zone)
            ),
            None => format!(
                "{}, from {}",
                state::day_long(state::day_of(run.started_at, self.zone)),
                state::clock(run.started_at, self.zone)
            ),
        };
        let kv = |key: &str, value: String| {
            div()
                .flex()
                .gap(px(5.))
                .text_size(px(12.5))
                .child(div().text_color(c(t.t2)).child(key.to_string()))
                .child(value)
        };
        let section = |title: &str, note: Option<&str>| {
            div()
                .flex()
                .gap(px(6.))
                .pt(px(12.))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(t.t2))
                .child(title.to_string())
                .when_some(note.map(str::to_string), |d, n| {
                    d.child(div().font_weight(FontWeight::NORMAL).child(n))
                })
        };
        let mut earlier: Vec<&ScheduleRun> = runs
            .iter()
            .filter(|r| r.schedule == schedule && r.id != run_id)
            .collect();
        earlier.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        let paused = s.paused;
        let can_run = self.can_run(schedule, runs);
        let edited = s.clone();
        div()
            .p(px(16.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(s.name.clone()),
                    )
                    .when_some(kind, |d, k| d.child(ui::mark(k, t, 11.)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(c(match kind {
                                Some(k) => ui::word_ink(k, t),
                                None => t.t2,
                            }))
                            .child(words.clone()),
                    )
                    .child(div().text_size(px(12.5)).text_color(c(t.t2)).child(range))
                    .child(div().flex_1())
                    .when_some(tab, |d, tab| {
                        let working = run.ended_at.is_none();
                        d.child(
                            ui::pill(
                                ("its-terminal", u64::from(tab)),
                                &if working {
                                    format!("{words}, open it")
                                } else {
                                    "Its terminal".to_string()
                                },
                                t,
                            )
                            .h(px(28.))
                            .child(if working {
                                format!(
                                    "{words}, tab {}, open it",
                                    qoop_tui::mux::daemon_terminal(tab)
                                )
                            } else {
                                "Its terminal".to_string()
                            })
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open(Page::Session(tab), window, cx)
                                },
                            )),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(14.))
                    .child(kv("agent", s.agent.clone()))
                    .when(!space.is_empty(), |d| d.child(kv("space", space.clone())))
                    .child(kv("folder", state::tilde(&s.cwd)))
                    .when_some(state::took(run), |d, took| d.child(kv("took", took)))
                    .when_some(run.terminal, |d, tab| {
                        d.child(kv(
                            "tab",
                            qoop_tui::mux::daemon_terminal(tab as u32).to_string(),
                        ))
                    })
                    .child(kv(
                        if s.cron.is_some() { "cron" } else { "when" },
                        s.cron.clone().unwrap_or_else(|| s.when.clone()),
                    )),
            )
            .child(section("Summary", Some("its last message")))
            .child(div().text_size(px(13.)).text_color(c(t.t1)).child(
                run.summary.clone().unwrap_or_else(|| match run.ended_at {
                    None => "It is still running.".into(),
                    Some(_) => "It left no message.".into(),
                }),
            ))
            .when_some(
                self.schedule_page
                    .task
                    .as_ref()
                    .and_then(|(_, s)| s.as_ref())
                    .filter(|s| s.task.is_some()),
                |d, strip| {
                    d.child(section("Task", None))
                        .child(self.report_task(strip, t))
                },
            )
            .when_some(self.schedule_page.produced.as_ref(), |d, (_, at, read)| {
                let note = format!("in the folder now, as of {}", state::clock(*at, self.zone));
                d.child(section("Produced", Some(note.as_str())))
                    .child(match read {
                        None => quiet("Reading the folder.", t).into_any_element(),
                        Some(Err(why)) => quiet(why, t).into_any_element(),
                        Some(Ok(changes)) if changes.files.is_empty() => {
                            quiet(&format!("No changes against {}.", changes.base), t)
                                .into_any_element()
                        }
                        Some(Ok(changes)) => div()
                            .flex()
                            .flex_col()
                            .children(changes.files.iter().map(|f| {
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .text_size(px(12.5))
                                    .child(
                                        div().flex_1().font_family(MONO_FONT).child(f.path.clone()),
                                    )
                                    .when_some(crate::review::counts(f), |d, n| {
                                        d.child(div().text_color(c(t.t2)).child(n))
                                    })
                            }))
                            .into_any_element(),
                    })
            })
            .when(!earlier.is_empty(), |d| {
                d.child(section(&format!("Earlier runs of {}", s.name), None))
                    .children(earlier.into_iter().take(20).map(|r| {
                        let (kind, words) = self.run_words(r);
                        let run = r.clone();
                        let line = r
                            .summary
                            .as_deref()
                            .and_then(|m| m.lines().next())
                            .unwrap_or("")
                            .to_string();
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .py(px(3.))
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .w(px(96.))
                                    .flex_none()
                                    .font_family(MONO_FONT)
                                    .child(state::clock_at(r.started_at, self.now, self.zone)),
                            )
                            .child(
                                div()
                                    .w(px(130.))
                                    .flex_none()
                                    .text_color(c(match kind {
                                        Some(k) => ui::word_ink(k, t),
                                        None => t.t2,
                                    }))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(words),
                            )
                            .child(
                                div()
                                    .w(px(56.))
                                    .flex_none()
                                    .text_color(c(t.t2))
                                    .child(state::took(r).unwrap_or_default()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(line),
                            )
                            .child(
                                ui::link_button(("earlier", r.id as u64), "Report", t).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.open_run(schedule, &run, cx)
                                    }),
                                ),
                            )
                    }))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child(format!("{}, prompt: {}", s.when, s.prompt)),
                    )
                    .child(
                        ui::pill("edit-schedule", "Edit", t)
                            .h(px(28.))
                            .child("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_sheet(Some(&edited), window, cx)
                            })),
                    )
                    .child(
                        ui::pill("pause-schedule", if paused { "Resume" } else { "Pause" }, t)
                            .h(px(28.))
                            .child(if paused { "Resume" } else { "Pause" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let write = if paused { Write::Resume } else { Write::Pause };
                                this.press(schedule, write, cx);
                            })),
                    )
                    .child(match can_run {
                        true => ui::pill("run-now", "Run now", t)
                            .h(px(28.))
                            .child("Run now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.press(schedule, Write::Run, cx)
                            })),
                        false => ui::pill(
                            "run-now",
                            "Run now, unavailable: its last run is still working",
                            t,
                        )
                        .h(px(28.))
                        .tab_stop(false)
                        .opacity(0.5)
                        .child("Run now"),
                    }),
            )
            .when_some(
                self.schedule_page.said.get(&schedule).cloned(),
                |d, said| {
                    d.child(
                        div()
                            .id(("schedule-said", schedule as u64))
                            .role(Role::Status)
                            .aria_label(said.clone())
                            .text_size(px(12.5))
                            .text_color(c(t.t1))
                            .child(said),
                    )
                },
            )
            .child(quiet("A missed run is caught up once, within 7 days.", t))
            .into_any_element()
    }

    fn report_task(&self, strip: &state::Strip, t: &Theme) -> impl IntoElement {
        let goal = strip
            .task
            .as_ref()
            .map(|k| k.goal.clone())
            .unwrap_or_default();
        let evidence: Vec<String> = strip.evidence().into_iter().map(|e| e.text).collect();
        let done = strip
            .events
            .iter()
            .rev()
            .find(|e| e.kind == "done" || e.kind == "closed")
            .map(|e| e.body.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .text_size(px(12.5))
            .child(div().text_color(c(t.t1)).child(goal))
            .children(strip.steps.iter().map(|step| {
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().flex_1().child(step.text.clone()))
                    .child(
                        div().font_family(MONO_FONT).text_color(c(t.t2)).child(
                            step.done_at
                                .map(|at| state::clock(at, self.zone))
                                .unwrap_or_default(),
                        ),
                    )
            }))
            .when(!evidence.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(c(t.t2))
                        .child(format!("Evidence: {}", evidence.join("; "))),
                )
            })
            .when_some(done, |d, done| {
                d.child(div().text_color(c(t.t2)).child(format!("Done: {done}")))
            })
    }

    /// New schedule and Edit: name, when (the TUI's presets), agent, space, folder, prompt, the
    /// first run read back, Save.
    fn schedule_sheet(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(sheet) = self.schedule_page.sheet.as_ref() else {
            return div().into_any_element();
        };
        let first = match self.sheet_timing(sheet, cx) {
            Ok(timing) => match qoop_tui::schedules::first_run(&timing, self.now, self.zone) {
                Some(at) => format!("First run: {}", state::clock_at(at, self.now, self.zone)),
                None => "A watch: its first check notes what is there, then new lines start a run."
                    .to_string(),
            },
            Err(why) => format!("When: {why}"),
        };
        let agent = sheet.agent.clone();
        let row = |label: &str, field: &Entity<Field>| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(70.))
                        .flex_none()
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .p(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ca(t.line))
                        .child(field.clone()),
                )
        };
        let preset = sheet.preset;
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if sheet.editing.is_some() {
                        "Edit schedule"
                    } else {
                        "New schedule"
                    }),
            )
            .child(row("Name", &sheet.name))
            .child(
                div()
                    .id("presets")
                    .role(Role::RadioGroup)
                    .aria_label("When")
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(Preset::ALL.iter().enumerate().map(|(n, p)| {
                        let p = *p;
                        ui::chip(("preset", n as u64), p.label(), None, None, p == preset, t)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(s) = this.schedule_page.sheet.as_mut() {
                                    s.preset = p;
                                }
                                cx.notify();
                            }))
                    })),
            )
            .when(
                matches!(preset, Preset::Once | Preset::Weekdays | Preset::EveryDay),
                |d| d.child(row("Time", &sheet.time)),
            )
            .when(matches!(preset, Preset::Custom | Preset::OnChange), |d| {
                d.child(row(
                    if preset == Preset::Custom {
                        "Cron"
                    } else {
                        "Command"
                    },
                    &sheet.cron,
                ))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(70.))
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child("Agent"),
                    )
                    .child(
                        ui::pill("sheet-agent", &format!("Agent, {agent}"), t)
                            .h(px(28.))
                            .child(agent.clone())
                            .on_click(cx.listener(|this, _, _, cx| {
                                // A scheduled run starts with its prompt, so only agents that take one.
                                let mut names: Vec<String> = this
                                    .offers
                                    .iter()
                                    .filter(|(_, command)| state::takes_prompt(command))
                                    .map(|(name, _)| name.clone())
                                    .collect();
                                if let Some(s) = this.schedule_page.sheet.as_mut() {
                                    if let Some(own) = s.original.as_ref().map(|o| o.agent.clone())
                                        && !names.contains(&own)
                                    {
                                        names.push(own);
                                    }
                                    let at = names.iter().position(|n| *n == s.agent).unwrap_or(0);
                                    if let Some(next) = names.get((at + 1) % names.len().max(1)) {
                                        s.agent = next.clone();
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(row("Folder", &sheet.folder))
            .child(row("Prompt", &sheet.prompt))
            .child(quiet(&first, t))
            .when(self.schedule_page.via.is_some(), |d| {
                d.child(quiet(
                    "Times here are read on this Mac's clock; the other machine runs them on its own.",
                    t,
                ))
            })
            .when_some(sheet.refused.clone(), |d, why| {
                d.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(c(t.fail_text))
                        .child(why),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(div().flex_1())
                    .child(ui::kbd("esc", t))
                    .child(
                        ui::pill("sheet-save", "Save", t)
                            .h(px(30.))
                            .child("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_sheet(cx))),
                    ),
            );
        ui::sheet("schedule-sheet", "Schedule", t, body.into_any_element()).into_any_element()
    }
}

fn quiet(text: &str, t: &Theme) -> gpui::Div {
    div()
        .text_size(px(12.5))
        .text_color(c(t.t2))
        .child(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, started_at: i64, result: &str, ended_at: Option<i64>) -> ScheduleRun {
        serde_json::from_value(serde_json::json!({
            "id": id, "schedule": 1, "slot": 0, "started_at": started_at,
            "result": result, "ended_at": ended_at,
        }))
        .expect("run")
    }

    /// Run now waits while any run is claimed or still works, a newer skipped row or not.
    #[test]
    fn run_now_waits_for_any_open_run() {
        assert_eq!(NO_TAB_FOR, qoop_store::schedule::NO_TAB_FOR);
        assert!(!running_now(&[], 1, 100));
        // Claimed with no tab: open for its first two minutes, then never started.
        assert!(running_now(
            &[run(1, 10, "claimed", None)],
            1,
            10 + NO_TAB_FOR - 1
        ));
        assert!(!running_now(
            &[run(1, 10, "claimed", None)],
            1,
            10 + NO_TAB_FOR
        ));
        assert!(running_now(&[run(1, 10, "started", None)], 1, 100));
        assert!(running_now(&[run(1, 10, "claimed", None)], 1, 100));
        assert!(!running_now(&[run(1, 10, "started", Some(20))], 1, 100));
        assert!(
            running_now(
                &[
                    run(1, 10, "started", None),
                    run(2, 30, "skipped: busy", Some(30))
                ],
                1,
                100
            ),
            "a newer skipped row does not end the run that still works"
        );
        assert!(
            !running_now(&[run(1, 10, "started", None)], 2, 100),
            "another schedule's"
        );
        assert_eq!(
            write_refusal("still_running", "This Mac"),
            "Its last run is still working. Run now is back when it ends."
        );
        assert_eq!(
            write_refusal("unknown variant `Change`", "staging"),
            "qoop on staging is too old for this. Update it there first."
        );
    }
}
