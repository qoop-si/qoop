//! The window: routing between pages, the shell around them (`shell.rs`), the prefix keys, and
//! what the link brings in.
//!
//! Pages are the Inbox (where the window opens), a Session (one terminal; qass's place is its
//! own terminal's), qass while it does not run, Schedules, Settings and a Computer. The prefix from `config.toml` (default
//! `ctrl-a`) belongs to the app: `ctrl-a c` New agent, `ctrl-a i` the Inbox (and back),
//! `ctrl-a tab` the agent that has waited longest, the prefix twice sends it to the terminal on
//! screen. Cmd-J (next waiting) and Cmd-N (New agent) are the app's and never reach a terminal.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, Bounds, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, Pixels, Render, Role, Subscription, TitlebarOptions, UTF16Selection,
    Window, WindowAppearance, WindowBounds, WindowOptions, div, linear_color_stop, linear_gradient,
    point, prelude::*, px, size,
};
use qoop_domain::Viewport;
use qoop_runtime::avatar::Avatar;
use qoop_runtime::control::{
    AgentMenu, AskInfo, ControlRequest, ControlResponse, EventMessage, TerminalStream,
};
use qoop_runtime::daemon::SnapshotWire;
use qoop_runtime::schedule::Zone;
use qoop_tui::seen;
use tokio::sync::mpsc::UnboundedSender;

use crate::board::BoardPage;
use crate::element::{BACKGROUND, Cells, FOREGROUND, cells};
use crate::keys::{self, Press};
use crate::link::{self, Down, Latest, Up};
use crate::newwork::NewWork;
use crate::question::{OwnAnswer, Said};
use crate::review::Review;
use crate::shell::{self, Edge};
use crate::state::{self, Model};
use crate::theme::{TOP_GLOW, Theme, UI_FONT, c, ca};
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Inbox,
    Session(TerminalStream),
    Qass,
    Schedules,
    Settings,
    /// A computer by its machine number: how it is reached, and its agents.
    Computer(usize),
}

/// A terminal on screen, with what its pane remembers between frames.
pub struct Term {
    pub terminal: TerminalStream,
    pub screen: Option<Rc<SnapshotWire>>,
    pub(crate) sent: Rc<Cell<(u16, u16)>>,
    pub(crate) viewport: Rc<Cell<Option<Viewport>>>,
    /// Where the pane was drawn last, for a mouse position to a cell.
    pub(crate) area: Rc<Cell<Bounds<Pixels>>>,
    /// Rows scrolled back from the live screen by the wheel, and the page of scrollback shown
    /// then (`Scrollback`); 0 is the live screen.
    pub(crate) scroll: usize,
    pub(crate) back: Option<Rc<SnapshotWire>>,
    /// The rows there were at the last answer: `scroll` counts back from there.
    pub(crate) total: u64,
    /// Rows of scrollback held around the view, by the number of the first, so a wheel step
    /// inside them is drawn in the next frame with no round trip.
    pub(crate) block: Option<(u64, Vec<String>)>,
    /// The oldest row the daemon still keeps, once an answer showed it.
    pub(crate) floor: u64,
    /// The one `Scrollback` out, by id, and the `up` it asked: any other answer is stale.
    pub(crate) asked: Option<(String, usize)>,
    /// The part of a row the wheel moved that has not scrolled yet, from 0 up to 1.
    pub(crate) wheel_left: f32,
    /// The rows `back` is drawn up by, under one: it then starts a row early, so the part of a
    /// row the wheel moved shows as pixels.
    pub(crate) lift: f32,
    /// Cells selected with the mouse, anchor then end, as (row, column) of what is shown.
    pub(crate) selection: Option<((usize, usize), (usize, usize))>,
}

impl Term {
    pub fn new(terminal: TerminalStream) -> Term {
        Term {
            terminal,
            screen: None,
            sent: Rc::new(Cell::new((0, 0))),
            viewport: Rc::new(Cell::new(None)),
            area: Rc::new(Cell::new(Bounds::default())),
            scroll: 0,
            back: None,
            total: 0,
            block: None,
            floor: 0,
            asked: None,
            wheel_left: 0.,
            lift: 0.,
            selection: None,
        }
    }

    /// What the pane shows: the scrollback page while scrolled, else the live screen.
    pub fn shown(&self) -> Option<Rc<SnapshotWire>> {
        match (&self.back, self.scroll) {
            (Some(back), 1..) => Some(back.clone()),
            _ => self.screen.clone(),
        }
    }
}

pub struct Desk {
    pub model: Model,
    pub page: Page,
    /// The page `ctrl-a i` and Escape go back to from the Inbox.
    pub(crate) back: Page,
    /// The space of the last agent page on screen: where New agent opens when the page on
    /// screen has no space of its own.
    pub(crate) worked_in: Option<i64>,
    /// The chip over the Inbox's list, the card with its ring, and where in the list it was.
    pub(crate) inbox_filter: crate::inbox::Filter,
    pub(crate) inbox_cursor: Option<state::Slot>,
    pub(crate) cursor_at: usize,
    /// What this client answered itself: agent menus leave no card, so Answered today reads it.
    pub(crate) own_answers: Vec<qoop_tui::inbox::OwnAnswer>,
    /// When the person's day began, for Answered today; looked up again once a day.
    pub(crate) day_start: i64,
    /// The spaces the sidebar lists in full instead of folded.
    pub(crate) expanded: BTreeSet<i64>,
    /// The sidebar's and the drawers' widths as dragged, the window's width as last drawn, and
    /// the divider being dragged.
    pub(crate) side_w: f32,
    pub(crate) drawer_w: f32,
    pub(crate) review_w: f32,
    pub(crate) win_w: f32,
    pub(crate) win_h: f32,
    pub(crate) drag: Option<Edge>,
    /// When the person last typed into each terminal: a held reply is never typed into a line
    /// they began after it was held.
    pub(crate) typed_at: HashMap<TerminalStream, Instant>,
    /// Words typed for a card whose field closed or whose reply was not sent, kept per card.
    pub(crate) drafts: HashMap<state::Slot, String>,
    /// Finished and Answered today listed in full, not their first lines.
    pub(crate) quiet_all: bool,
    /// One focus handle per Inbox card: the ring follows the keyboard into a card's controls.
    pub(crate) card_focus: std::cell::RefCell<HashMap<state::Slot, FocusHandle>>,
    pub sheet: Option<NewWork>,
    /// New work closed with a task typed in it, given back on the next open.
    pub work_draft: Option<NewWork>,
    /// The terminal the keys go to, and the other panes of its tab, read only.
    pub term: Option<Term>,
    pub others: Vec<Term>,
    /// The task strip of the session on screen, and the read that asked for it.
    pub strip: Option<state::Strip>,
    pub(crate) strip_ask: Option<String>,
    /// The strip drawn as a vertical timeline.
    pub timeline: bool,
    /// The task strip's drawer beside the terminal is open (closed until asked for).
    pub(crate) task_open: bool,
    /// What the agent page measures of its column (`element::Gauge`).
    pub(crate) gauge: Rc<crate::element::Gauge>,
    pub review: Option<Review>,
    pub review_focus: FocusHandle,
    /// Picks waiting out their 3 s, and what became of each sent one, per agent.
    pub holds: state::Holds,
    pub said: HashMap<state::Slot, Said>,
    /// The pick just held: where it is drawn, the list or room around it scrolls it into view,
    /// once (`Desk::reveal_once`).
    pub(crate) reveal: std::cell::RefCell<Option<state::Slot>>,
    /// Machines whose qoop refused a typed answer as unknown: no typed answers there.
    pub(crate) too_old: BTreeSet<usize>,
    /// The owner's own words for an agent's answer field, while it is open.
    pub(crate) own: Option<OwnAnswer>,
    /// The question card opened in place on the Session page; `None` opens the oldest.
    pub(crate) card_open: Option<String>,
    /// qass's cards waiting on the owner.
    pub asks: Vec<AskInfo>,
    /// The last few cards that ended, newest first: with `asks`, what Answered today reads.
    pub ended: Vec<AskInfo>,
    /// What starting qass said, while it starts or when it could not.
    pub(crate) qass_said: Option<String>,
    /// The Board (the Inbox's second view), and what the last line to qass said.
    pub(crate) board: BoardPage,
    pub(crate) ask_said: Option<String>,
    /// The one timer of the holds is running.
    pub(crate) ticking: bool,
    /// The person's `config.toml` and their pet's file: the settings page reads and writes
    /// these (tests point them at their own).
    pub config_path: std::path::PathBuf,
    pub pet_path: std::path::PathBuf,
    pub settings: crate::settings::SettingsPage,
    /// A start of qass asked from here holds Start greyed until this unix second.
    pub(crate) qass_starting_until: Option<i64>,
    /// Since when each machine has not answered, and when this Mac's daemon went away.
    pub reach: state::Reach,
    pub lost_at: Option<i64>,
    /// What Start qoop said while This Mac's daemon is down: starting, or why it could not.
    pub(crate) qoop_said: Option<String>,
    /// The question on screen that another client answered: its closed options, greyed,
    /// under who answered, until dismissed or a new question.
    pub(crate) closed_card: Option<(TerminalStream, AgentMenu, String)>,
    /// Other machines' pets, as their daemons drew them, and the reads still out.
    pub(crate) machine_pets: HashMap<usize, Vec<state::PetRect>>,
    pet_asks: HashMap<String, usize>,
    /// Why each machine's link is down, as the mux said it (an ssh error): a tooltip, never a name.
    pub(crate) link_reasons: HashMap<usize, String>,
    /// The requests out to another machine, by id: an answer to any of them is that machine
    /// heard from.
    sent_to: std::cell::RefCell<HashMap<String, usize>>,
    /// When each machine was last asked for a word (its pet).
    pinged: HashMap<usize, i64>,
    /// The machine states last drawn: the clock draws again when one changes.
    drawn_states: Vec<state::MachineState>,
    /// The marks' clock: the frame of the working breath, and whether its timer runs.
    pub(crate) beat: shell::Beat,
    /// No `qoop` on this Mac (`state::qoop_bin`), looked at when the window opens.
    pub(crate) qoop_missing: bool,
    /// The agent list came at least once: before it, the Inbox and the sidebar draw skeletons.
    pub(crate) listed: bool,
    /// The rows are this connection's: a fresh list has come since it began and was checked
    /// against what is held. False from a drop on, so a pick is never sent by a number that
    /// may be another agent's after a restart or a reconnect.
    pub(crate) fresh: bool,
    /// The schedules page: machine, runs, day, the report open, the sheet.
    pub schedule_page: crate::schedules::SchedulePage,
    /// The person's `config.toml`: the prefix byte and the theme (auto follows the system).
    prefix: u8,
    theme_pref: qoop_tui::config::Theme,
    /// The system's appearance, for a theme of auto.
    appearance: WindowAppearance,
    /// `config.toml`'s time and size when last read: the window follows a change to it.
    config_stamp: Option<(SystemTime, u64)>,
    pub dark: bool,
    prefix_pending: bool,
    pub up: UnboundedSender<Up>,
    latest: Latest,
    pub connected: bool,
    pub focus: FocusHandle,
    pub term_focus: FocusHandle,
    /// Kept for the window's life: focus that falls out of the frame comes back (WEB-09).
    _focus_lost: Subscription,
    /// Text the input method composes for the terminal; what it commits is typed.
    marked: Option<String>,
    /// The person's last key or click here, for seen.
    last_input: Option<Instant>,
    look: Option<seen::Look>,
    pub now: i64,
    pub zone: Zone,
    pub my_pet: Option<Avatar>,
    pub(crate) cells: Cells,
    /// The agents New work offers: (name, command).
    pub offers: Vec<(String, String)>,
    /// The agents a scheduled run or qass can start, qass's own first, as the TUI's form lists
    /// them (`qoop_tui::schedules::agents`): the schedule sheet and "qass runs on" read it.
    pub(crate) agent_names: Vec<String>,
    /// New work asked for: the create's id, the line its terminal gets, and the space. Kept
    /// here, not on the sheet, so a sheet closed while it starts still gets its line.
    pub pending: Option<(String, String, Option<i64>)>,
}

/// What the status line says once Start qoop is pressed, until the window is connected.
const STARTING_QOOP: &str = "Starting qoop.";

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Desk {
    pub fn new(
        up: UnboundedSender<Up>,
        latest: Latest,
        config: &qoop_tui::config::Read,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // A focused control that leaves the frame (a Report link that becomes the shown run,
        // Retry now once the machine is back, an option whose question was answered) leaves
        // GPUI dispatching keys above the desk, where no listener hears them, `ctrl-a` too.
        let focus_lost = cx.on_focus_lost(window, |this: &mut Desk, window, cx| {
            this.focus_home(window, cx)
        });
        let mut offered: Vec<(String, String)> = ["claude", "codex", "opencode"]
            .map(|a| (a.to_string(), a.to_string()))
            .to_vec();
        for l in &config.agents {
            if !offered.iter().any(|(name, _)| *name == l.name) {
                offered.push((l.name.clone(), l.command.clone()));
            }
        }
        let theme_pref = config.config.theme;
        // The owner's own files, read from a run and never from a test (a test points the window
        // at its own): This Mac's hand id is read here and not made, so another Mac's hand is
        // told apart from this Mac's own from the first list on; the shared hand rules make the
        // file once a remote row or a hand needs it, as the terminal interface does.
        let owner = !cfg!(test);
        let hand_dir = owner.then(qoop_runtime::paths::qass_data_dir).flatten();
        let hand_id = hand_dir
            .as_ref()
            .and_then(|dir| {
                std::fs::read_to_string(dir.join(qoop_runtime::paths::HAND_ID_FILE)).ok()
            })
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        let nowhere = std::path::PathBuf::from("/nonexistent/qoop");
        let (config_path, pet_path) = if owner {
            (
                qoop_tui::config::path(),
                qoop_runtime::avatar::avatar_path(),
            )
        } else {
            (nowhere.join("config.toml"), nowhere.join("avatar.json"))
        };
        Desk {
            model: Model {
                hand: qoop_tui::answers::HandInput {
                    hand_id,
                    dir: hand_dir,
                    ..Default::default()
                },
                ..Model::default()
            },
            page: Page::Inbox,
            back: Page::Inbox,
            worked_in: None,
            inbox_filter: Default::default(),
            inbox_cursor: None,
            cursor_at: 0,
            own_answers: Vec::new(),
            day_start: 0,
            expanded: BTreeSet::new(),
            side_w: shell::SIDE_DEFAULT,
            drawer_w: shell::DRAWER_DEFAULT,
            review_w: shell::REVIEW_DEFAULT,
            win_w: 1440.,
            win_h: 900.,
            drag: None,
            typed_at: HashMap::new(),
            drafts: HashMap::new(),
            quiet_all: false,
            card_focus: Default::default(),
            sheet: None,
            work_draft: None,
            term: None,
            others: Vec::new(),
            strip: None,
            strip_ask: None,
            timeline: false,
            task_open: false,
            gauge: Default::default(),
            review: None,
            review_focus: cx.focus_handle(),
            holds: state::Holds::default(),
            said: HashMap::new(),
            reveal: Default::default(),
            too_old: BTreeSet::new(),
            own: None,
            card_open: None,
            asks: Vec::new(),
            ended: Vec::new(),
            qass_said: None,
            board: BoardPage::new(cx),
            ask_said: None,
            ticking: false,
            schedule_page: Default::default(),
            config_path,
            pet_path,
            settings: Default::default(),
            qass_starting_until: None,
            reach: state::Reach::default(),
            lost_at: None,
            qoop_said: None,
            closed_card: None,
            machine_pets: HashMap::new(),
            pet_asks: HashMap::new(),
            link_reasons: HashMap::new(),
            sent_to: Default::default(),
            pinged: HashMap::new(),
            drawn_states: Vec::new(),
            beat: Default::default(),
            listed: false,
            fresh: false,
            qoop_missing: state::qoop_bin().is_none(),
            prefix: config.config.prefix,
            theme_pref,
            appearance: window.appearance(),
            config_stamp: None,
            dark: dark_for(theme_pref, window.appearance()),
            prefix_pending: false,
            up,
            latest,
            connected: false,
            focus,
            term_focus: cx.focus_handle(),
            _focus_lost: focus_lost,
            marked: None,
            last_input: None,
            look: None,
            now: unix_now(),
            zone: qoop_runtime::schedule::system_zone(),
            my_pet: owner.then(qoop_runtime::avatar::load).flatten(),
            cells: cells(cx, 12.),
            offers: state::worktree_agents(&offered),
            agent_names: qoop_tui::schedules::agents(&config.agents, &config.qass_agent),
            pending: None,
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::pick(self.dark)
    }

    pub fn ask(&self, request: ControlRequest) {
        let m = state::request_machine(&request);
        if m != 0 {
            // A `Via` is answered by the far daemon under the inner request's id.
            let id = match &request {
                ControlRequest::Via { request: inner, .. } => inner.request_id(),
                other => other.request_id(),
            };
            self.sent_to.borrow_mut().insert(id.to_string(), m);
        }
        let _ = self.up.send(Up::Request(Box::new(request)));
    }

    /// Take in what the link said.
    pub fn apply(&mut self, down: Down, window: &mut Window, cx: &mut Context<Self>) {
        if let Down::Event(
            EventMessage::AgentChanged { terminal }
            | EventMessage::MessagesChanged { terminal, .. }
            | EventMessage::Listen { terminal, .. }
            | EventMessage::Focus { terminal, .. },
        ) = &down
        {
            self.heard_from(qoop_tui::mux::machine_of_terminal(*terminal));
        }
        match down {
            Down::Connected => {
                self.connected = true;
                self.fresh = false;
                self.lost_at = None;
                self.qoop_said = None;
                self.reach.forget_links();
                // A New work asked for before the connection dropped: asked again, under its id.
                self.ask_pending();
                // A new connection, maybe a new daemon: say the size again. A `Scrollback` out
                // went with the old one and is never answered: the next wheel step asks again.
                for term in self.panes_mut() {
                    term.sent.set((0, 0));
                    term.asked = None;
                }
                self.ask_strip();
                self.ask_inbox();
                self.board_reconnected();
            }
            Down::Disconnected(why) => {
                if self.connected {
                    eprintln!("qoop-desktop: {why}; connecting again");
                    self.lost_at = Some(self.now);
                }
                self.connected = false;
                self.fresh = false;
                self.sending_dropped();
            }
            Down::Screens => {
                let mut screens =
                    std::mem::take(&mut *self.latest.lock().unwrap_or_else(|e| e.into_inner()));
                // A screen from another machine's terminal is that machine heard from.
                for terminal in screens.keys() {
                    self.heard_from(qoop_tui::mux::machine_of_terminal(*terminal));
                }
                for term in self.term.iter_mut().chain(self.others.iter_mut()) {
                    if let Some(screen) = screens.remove(&term.terminal) {
                        term.screen = Some(Rc::new(screen));
                    }
                }
            }
            Down::Answer(answer) => {
                // A refusal may be the mux's own ("staging went offline"): only an answer counts.
                let to = self.sent_to.borrow_mut().remove(answer.request_id());
                if let (Some(m), false) = (to, matches!(*answer, ControlResponse::Failed { .. })) {
                    self.heard_from(m);
                }
                self.answer(*answer, window, cx)
            }
            Down::Event(EventMessage::Focus { terminal, .. }) => {
                self.open(Page::Session(terminal), window, cx);
                window.activate_window();
            }
            Down::Event(EventMessage::TasksChanged) => {
                self.ask_strip();
                self.ask_watch();
                self.board_changed();
            }
            Down::Event(EventMessage::AsksChanged) => self.ask_inbox(),
            Down::Event(EventMessage::SchedulesChanged) => self.refresh_remote(),
            Down::Event(_) => {}
        }
    }

    fn answer(&mut self, answer: ControlResponse, window: &mut Window, cx: &mut Context<Self>) {
        if self.answer_reply(&answer, cx)
            || self.report_came(&answer, window, cx)
            || self.remote_setting_came(&answer)
            || self.board_answered(&answer, cx)
        {
            return;
        }
        match answer {
            ControlResponse::Agents {
                mut agents, splits, ..
            } => {
                agents.extend(state::unreached_rows(
                    &self.model.rows,
                    &agents,
                    &self.model.spaces,
                ));
                let qass_was = self.qass_terminal();
                let before = std::mem::replace(&mut self.model.rows, agents);
                self.model.splits = splits;
                // A new question on a row: "Sent: Yes, 18:16" was the old one's.
                for terminal in state::new_menus(&before, &self.model.rows) {
                    let slot = state::Slot::Agent(terminal);
                    if !matches!(self.said.get(&slot), Some(Said::Sending { .. })) {
                        self.said.remove(&slot);
                    }
                }
                self.note_hand();
                self.listed = true;
                self.answered_elsewhere(&before);
                self.rows_changed();
                self.menus_changed(cx);
                self.fresh = true;
                // qass started or woke while its page is open: its terminal comes on.
                if self.qass_terminal().is_some() {
                    self.qass_starting_until = None;
                }
                // qass's place follows qass both ways: its agent page when qass starts or wakes,
                // "Start it" once its agent leaves its terminal (a plain shell is not qass). A
                // terminal of This Mac's qass space is qass's place even when this window never
                // saw qass run in it.
                let place = match self.page {
                    Page::Qass => true,
                    Page::Session(t) => Some(t) == qass_was || self.model.in_qass_space(t),
                    _ => false,
                };
                if place && self.page != self.qass_terminal().map_or(Page::Qass, Page::Session) {
                    self.open(Page::Qass, window, cx);
                }
                // New agent's agent is listed: the sheet closes on it.
                self.follow_new_agent(window, cx);
            }
            ControlResponse::Asks { live, ended, .. } => {
                self.asks = live;
                self.ended = ended;
                self.note_hand();
            }
            ControlResponse::SleepSettings { problem, .. } => {
                self.read_settings();
                self.settings.said = Some(match problem {
                    Some(why) => why,
                    None => format!(
                        "Sleep: written by qoop's daemon, {}",
                        state::clock(self.now, self.zone)
                    ),
                });
            }
            ControlResponse::Tasks {
                id,
                tasks,
                events,
                item,
                ..
            } if self.strip_ask.as_deref() == Some(id.as_str()) => {
                self.strip = Some(state::Strip::of(tasks, events, item));
            }
            ControlResponse::Changes {
                id,
                root,
                base,
                files,
                diff,
                cut,
                ..
            } => {
                if let Some(review) = self.review.as_mut().filter(|r| r.id == id) {
                    review.read = Some(Ok(crate::review::Changes {
                        root,
                        base,
                        files,
                        diffs: crate::review::split_diff(&diff),
                        cut,
                    }));
                }
            }
            ControlResponse::Scrollback {
                id,
                from,
                total,
                lines,
                ..
            } => self.scrolled_back(&id, from, total, lines),
            ControlResponse::Workspaces { workspaces, .. } => {
                self.link_reasons = state::link_reasons(&workspaces);
                let back = self.reach.note(&workspaces, self.now);
                // An ask a dropped link took with it is answered by nobody: ask again once up.
                let down = &self.reach.down_since;
                self.pet_asks.retain(|_, m| !down.contains_key(m));
                self.model.spaces = workspaces;
                self.note_hand();
                self.ping_machines();
                self.machines_back(&back);
            }
            ControlResponse::Pet { id, rects } => {
                if let Some(m) = self.pet_asks.remove(&id) {
                    self.pet_asks.retain(|_, a| *a != m);
                    self.machine_pets.insert(m, rects);
                }
            }
            ControlResponse::Schedules { id, schedules } => self.schedules_came(&id, schedules),
            ControlResponse::ScheduleRuns { id, runs } => {
                self.runs_came(&id, runs);
            }
            ControlResponse::QassWatch { on, .. } => {
                self.model.qass_watch = Some(on);
                self.note_hand();
            }
            ControlResponse::TerminalCreated { id, terminal } => {
                self.started(&id, terminal, window, cx)
            }
            ControlResponse::Failed { id, reason } => {
                if let Some(review) = self.review.as_mut().filter(|r| r.id == id) {
                    review.read = Some(Err(reason));
                } else if let Some(m) = self.pet_asks.remove(&id) {
                    // A qoop there from before pets refuses the ask as unknown: that refusal is
                    // its daemon answering, so it counts as heard, and it is asked again on
                    // the clock like any other. The mux's own refusals (a link gone) never say
                    // that.
                    if reason.contains("unknown variant") {
                        self.pet_asks.retain(|_, a| *a != m);
                        self.heard_from(m);
                    }
                } else if self.strip_ask.as_deref() == Some(id.as_str()) {
                    // A daemon from before a window could read a tab's task: no strip.
                    self.strip = Some(state::Strip::default());
                } else if let Some(term) = self.panes_mut().find(|t| t.asked_is(&id)) {
                    // The next wheel step asks again.
                    term.asked = None;
                } else {
                    self.refused(&id, reason);
                }
            }
            _ => {}
        }
    }

    /// Go to a page, showing its terminal's screens and dropping the last one's. The Inbox opened
    /// from a session keeps that session's terminal shown behind it, so coming back is the same
    /// screen, scrollback and size, and the keys never reach it meanwhile.
    pub fn open(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        // qass's place is its agent page while it runs; the qass page says it does not.
        let page = if page == Page::Qass {
            self.qass_terminal().map_or(Page::Qass, Page::Session)
        } else {
            page
        };
        let keep = page == Page::Inbox
            && matches!(self.page, Page::Session(_) | Page::Inbox)
            && self.term.is_some();
        if page == Page::Inbox && self.page != Page::Inbox {
            self.back = self.page;
        }
        let terminal = match page {
            Page::Session(t) => Some(t),
            _ if keep => self.term.as_ref().map(|t| t.terminal),
            _ => None,
        };
        if self.term.as_ref().map(|t| t.terminal) != terminal {
            self.marked = None;
            self.review = None;
            self.strip = None;
        }
        self.page = page;
        if let Page::Session(t) = page {
            self.worked_in = self.model.row(t).map(|r| r.workspace).or(self.worked_in);
        }
        if page == Page::Settings {
            self.read_settings();
            self.ask_watch();
        }
        if page == Page::Schedules && self.schedule_page.via.is_none() {
            let ids: Vec<i64> = self.model.schedules.iter().map(|s| s.id).collect();
            self.ask_runs(&ids);
        }
        if !keep {
            self.set_panes(terminal);
        }
        if matches!(page, Page::Session(_)) {
            self.ask_strip();
        } else if !keep {
            self.strip = None;
            self.strip_ask = None;
        }
        if self.term_drawn() {
            window.focus(&self.term_focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Every card read, open or ended: what the Inbox and the hand rules read.
    pub(crate) fn cards(&self) -> Vec<AskInfo> {
        self.asks.iter().chain(&self.ended).cloned().collect()
    }

    /// The shared hand rules' input, brought up to date after a list, a card read or a switch.
    pub(crate) fn note_hand(&mut self) {
        let cards = self.cards();
        self.model.note_qass(&cards, self.now);
        qoop_tui::inbox_pane::day_start(&mut self.day_start, unix_now());
    }

    pub(crate) fn qass_terminal(&self) -> Option<TerminalStream> {
        self.model.qass_row().map(|r| r.terminal)
    }

    /// What `config.toml` changes in the window now: the prefix and the theme.
    pub(crate) fn reconfigure(&mut self, read: &qoop_tui::config::Read) {
        self.agent_names = qoop_tui::schedules::agents(&read.agents, &read.qass_agent);
        self.prefix = read.config.prefix;
        self.theme_pref = read.config.theme;
        self.dark = dark_for(self.theme_pref, self.appearance);
    }

    /// `config.toml` changed since it was last looked at (`qoop config set`, an editor):
    /// read it again. Returns whether it had.
    pub(crate) fn follow_config(&mut self) -> bool {
        let stamp = std::fs::metadata(&self.config_path)
            .and_then(|m| Ok((m.modified()?, m.len())))
            .ok();
        if stamp == self.config_stamp {
            return false;
        }
        self.config_stamp = stamp;
        if self.page == Page::Settings && self.settings.via.is_none() {
            self.read_settings();
        } else {
            self.reconfigure(&qoop_tui::config::read_from(&self.config_path));
        }
        true
    }

    /// The prefix as `config.toml` names it, for the key hints: `ctrl-b`.
    pub fn prefix_name(&self) -> String {
        qoop_tui::keys::name_of(self.prefix)
    }

    fn is_prefix(&self, k: &gpui::Keystroke) -> bool {
        let letter = match self.prefix {
            p @ 1..=26 => (b'a' + p - 1) as char,
            _ => return false,
        };
        k.modifiers.control
            && !k.modifiers.platform
            && !k.modifiers.alt
            && k.key.len() == 1
            && k.key.starts_with(letter)
    }

    /// Before anything else hears a key: the prefix and what follows it, the sheet's focus
    /// trap, and the person's last key for seen.
    fn capture_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.last_input = Some(Instant::now());
        let k = &event.keystroke;
        if self.prefix_pending {
            self.prefix_pending = false;
            cx.stop_propagation();
            if self.is_prefix(k) {
                if let Some(term) = &self.term
                    && self.term_drawn()
                    && self.connected
                {
                    let terminal = term.terminal;
                    let _ = self.up.send(Up::Input(terminal, vec![self.prefix]));
                    self.typed_at.insert(terminal, Instant::now());
                }
                return;
            }
            match keys::typed(k) {
                "c" => self.new_work(None, window, cx),
                // The closed question's card goes.
                "x" => {
                    self.closed_card = None;
                    cx.notify();
                }
                "tab" => self.next_waiting(window, cx),
                // The keyboard off the terminal, as the TUI's `ctrl-a a` puts it on the agents
                // column: `tab` then goes round the controls. Again: back on the terminal.
                "a" => {
                    if self.term_drawn() && !self.term_focus.is_focused(window) {
                        window.focus(&self.term_focus, cx);
                    } else {
                        window.focus(&self.focus, cx);
                    }
                    cx.notify();
                }
                // Changes, `D` for diff: a key the TUI leaves unbound, so one prefix key never
                // means two things across qoop. ponytail: the owner's naming pick.
                // The character the layout typed, or shift with the D key where none came.
                "D" => self.toggle_review(window, cx),
                "d" if k.modifiers.shift => self.toggle_review(window, cx),
                // The Inbox, and back to the page it was opened from.
                "i" => {
                    let to = if self.page == Page::Inbox {
                        self.back
                    } else {
                        Page::Inbox
                    };
                    self.open(to, window, cx);
                }
                _ => {}
            }
            return;
        }
        if self.is_prefix(k) {
            self.prefix_pending = true;
            cx.stop_propagation();
            return;
        }
        // The app's own chords, before any terminal or field hears them: Cmd-J the next agent
        // that waits, Cmd-N a new agent.
        if k.modifiers.platform && !k.modifiers.control && !k.modifiers.alt && !k.modifiers.shift {
            match k.key.as_str() {
                // Taken either way (the terminal never hears them); they act only with no sheet
                // open over the page.
                "j" => {
                    if self.sheet.is_none() && !self.schedule_sheet_open() {
                        self.next_waiting(window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "n" => {
                    if !self.schedule_sheet_open() {
                        self.new_work(None, window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if self.sheet.is_some() {
            self.sheet_key(event, window, cx);
            return;
        }
        if self.schedule_sheet_key(event, window, cx) {
            return;
        }
        // Enter and Escape in the Board's fields, which are theirs.
        if !k.modifiers.control
            && !k.modifiers.platform
            && !k.modifiers.alt
            && !keys::newline(k)
            && self.board_key(&k.key, window, cx)
        {
            cx.stop_propagation();
            return;
        }
        // Escape leaves the Inbox for the page before it, and the terminal never hears it: an
        // Escape typed into an agent interrupts its turn. Not from a field, which keeps its
        // keys.
        if self.page == Page::Inbox
            && self.back != Page::Inbox
            && keys::typed(k) == "escape"
            && !self.typing_in_field(window, cx)
        {
            self.open(self.back, window, cx);
            cx.stop_propagation();
            return;
        }
        self.answer_key(event, window, cx);
    }

    /// Focus back on the control that opened a sheet while it is still drawn, else on the
    /// window (WEB-06).
    pub(crate) fn focus_back(
        &self,
        back: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let to = back
            .filter(|b| self.focus.contains(b, window))
            .unwrap_or_else(|| self.focus.clone());
        window.focus(&to, cx);
    }

    /// Whether the page draws a terminal that can take the keys.
    pub(crate) fn term_drawn(&self) -> bool {
        self.term.is_some() && matches!(self.page, Page::Session(_))
    }

    /// Whether any text field of the window has the keys: the own words, New agent's task, the
    /// settings find, a schedule sheet's fields. The window's one-letter keys and digits are
    /// theirs then.
    pub(crate) fn typing_in_field(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let on = |field: &Entity<crate::field::Field>, window: &Window, cx: &Context<Self>| {
            field.read(cx).focus.is_focused(window)
        };
        let schedule = self
            .schedule_page
            .sheet
            .as_ref()
            .filter(|_| self.page == Page::Schedules)
            .is_some_and(|s| {
                [&s.name, &s.time, &s.cron, &s.folder, &s.prompt]
                    .into_iter()
                    .any(|f| on(f, window, cx))
            });
        schedule
            || on(&self.board.find, window, cx)
            || on(&self.board.tell, window, cx)
            || self
                .board
                .add
                .as_ref()
                .is_some_and(|a| on(&a.field, window, cx))
            || self.own.as_ref().is_some_and(|o| on(&o.field, window, cx))
            || self.sheet.as_ref().is_some_and(|s| on(&s.task, window, cx))
            || self
                .settings
                .find
                .as_ref()
                .is_some_and(|f| on(f, window, cx))
    }

    /// Whether a schedule sheet is open over the page.
    fn schedule_sheet_open(&self) -> bool {
        self.page == Page::Schedules && self.schedule_page.sheet.is_some()
    }

    /// The keys' resting place: an open sheet's first stop, else the terminal on screen, else
    /// the window. Where focus goes when its control leaves the frame or a panel closes.
    pub(crate) fn focus_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let schedule_sheet = self
            .schedule_page
            .sheet
            .as_ref()
            .filter(|_| self.page == Page::Schedules);
        let to = match (&self.sheet, schedule_sheet) {
            (Some(sheet), _) => sheet.stops[0].clone(),
            (None, Some(sheet)) => sheet.name.read(cx).focus.clone(),
            (None, None) if self.term_drawn() => self.term_focus.clone(),
            (None, None) => self.focus.clone(),
        };
        window.focus(&to, cx);
        cx.notify();
    }

    /// While a question card is on screen or the Inbox is: a digit picks its answer, `u` undoes
    /// the pick, Enter in the own-answer field holds those words. Other keys go on as before.
    fn answer_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.control || k.modifiers.platform || k.modifiers.alt {
            return;
        }
        if k.key == "enter" && !keys::newline(k) && self.own_enter(window, cx) {
            cx.stop_propagation();
            return;
        }
        // A text field keeps every key it is given.
        if self.typing_in_field(window, cx) {
            return;
        }
        // The Board has no cards to answer: its digits are the person's own.
        let on_inbox = self.page == Page::Inbox && !self.board.open;
        // `u` is Undo on the Inbox while anything is on hold (the focused card's pick, else the
        // newest), and on an agent's page, the terminal focused or not, only while a pick for that
        // agent is held and the menu it was made on is still up (a card's: while the card is
        // open). Once the menu is gone it is a letter for the terminal again, and anywhere else it
        // is one too: `ls -lu` never undoes anything.
        if keys::typed(k) == "u" {
            let slot = if self.page == Page::Inbox {
                let list = self.inbox();
                let items: Vec<_> = list
                    .items
                    .iter()
                    .filter(|i| self.inbox_filter_holds(i))
                    .collect();
                self.cursor_slot(&items)
                    .filter(|slot| self.holds.of(slot).is_some())
                    .map(Some)
                    .or_else(|| (!self.holds.held.is_empty()).then_some(None))
            } else if let Page::Session(terminal) = self.page {
                let asks = &self.asks;
                let menu_seal = self
                    .model
                    .row(terminal)
                    .and_then(|r| r.menu.as_ref())
                    .map(|m| m.seal);
                self.holds
                    .held
                    .iter()
                    .rev()
                    .find(|h| match &h.slot {
                        state::Slot::Agent(t) => {
                            *t == terminal && h.seal != 0 && menu_seal == Some(h.seal)
                        }
                        state::Slot::Card(id) => asks.iter().any(|a| {
                            a.card == *id && a.terminal == Some(terminal) && a.status == "open"
                        }),
                    })
                    .map(|h| Some(h.slot.clone()))
            } else {
                None
            };
            if let Some(slot) = slot {
                self.undo(slot.as_ref(), cx);
                cx.stop_propagation();
                return;
            }
        }
        let digit = match keys::typed(k) {
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                Some(d.as_bytes()[0] - b'0')
            }
            _ => None,
        };
        let taken = match digit {
            Some(d) if on_inbox && self.sheet.is_none() => self.inbox_digit(d, window, cx),
            // Digits only while the keys are the terminal's: a control keeps its own.
            Some(d)
                if self.own.is_none()
                    && self.card_offers().is_some()
                    && self.term_focus.is_focused(window) =>
            {
                self.digit(d, window, cx)
            }
            _ => false,
        };
        if taken {
            cx.stop_propagation();
        }
    }

    /// qass's switch read again: the CLI, the TUI or the phone may have turned it.
    pub(crate) fn ask_watch(&self) {
        self.ask(ControlRequest::QassWatch {
            id: link::fresh_id("qass"),
            on: None,
        });
    }

    /// qass's cards, read again.
    pub(crate) fn ask_inbox(&self) {
        self.ask(ControlRequest::AskRead {
            id: link::fresh_id("asks"),
            all: false,
        });
    }

    /// Keys nobody else took: Tab moves focus, Escape closes the changes drawer.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        match keys::typed(k) {
            "tab" if k.modifiers.shift => window.focus_prev(cx),
            "tab" => window.focus_next(cx),
            "escape" if self.review.is_some() => {
                self.review = None;
                window.focus(&self.term_focus, cx);
                cx.notify();
            }
            "escape" if self.page == Page::Schedules && self.schedules_escape(cx) => {}
            "escape" if self.page == Page::Settings && self.close_find(window, cx) => {}
            "/" if self.page == Page::Settings && self.settings.find.is_none() => {
                self.open_find(window, cx)
            }
            // Off the terminal with nothing open: Escape gives it the keys back. Not from the
            // own-answer field, which stays open: a digit then would go raw to the agent.
            "escape"
                if self.sheet.is_none()
                    && self.term_drawn()
                    && !self
                        .own
                        .as_ref()
                        .is_some_and(|own| own.field.read(cx).focus.is_focused(window)) =>
            {
                window.focus(&self.term_focus, cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// A key for the terminal on screen.
    pub(crate) fn term_key(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        if k.modifiers.platform && k.key == "q" {
            self.flush_holds();
            cx.quit();
            return;
        }
        if k.modifiers.platform && k.key == "c" {
            if self.copy(cx) {
                cx.stop_propagation();
            }
            return;
        }
        if k.modifiers.platform && k.key == "v" {
            self.paste(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Nothing typed while the daemon is away: it would land, late, in whatever runs then.
        let Some(term) = self.term.as_ref().filter(|_| self.connected) else {
            return;
        };
        let app_cursor = term
            .screen
            .as_ref()
            .is_some_and(|s| s.modes.application_cursor);
        let kitty = term.screen.as_ref().map_or(0, |s| s.modes.kitty_keyboard);
        let press = Press {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            ctrl: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            cmd: k.modifiers.platform,
            option_is_meta: false,
            kitty,
        };
        if let Some(bytes) = keys::encode(press, app_cursor) {
            let terminal = term.terminal;
            let _ = self.up.send(Up::Input(terminal, bytes));
            self.typed_at.insert(terminal, Instant::now());
            cx.stop_propagation();
            // Typing goes back to the live screen, as a terminal does.
            if term.scroll > 0 || term.selection.is_some() {
                self.back_to_live();
                cx.notify();
            }
        }
    }

    /// Machine `m` said something: a screen, an event about its terminals, an answer.
    fn heard_from(&mut self, m: usize) {
        if m != 0 {
            self.reach.hear(m, self.now);
        }
    }

    /// How machine `m` is reached now.
    pub(crate) fn machine_state(&self, m: usize) -> state::MachineState {
        self.reach
            .state(m, &self.model.spaces, self.now, self.lost_at)
    }

    /// "as of 15:58" for what is shown of a space whose machine is not reached.
    pub(crate) fn as_of(&self, workspace: i64) -> Option<String> {
        match self.machine_state(state::machine_of(workspace)) {
            state::MachineState::Stale { as_of } | state::MachineState::Behind { as_of } => {
                Some(format!("as of {}", state::clock(as_of, self.zone)))
            }
            state::MachineState::NeverReached => Some("since the window opened".into()),
            _ => None,
        }
    }

    /// The mark a row wears: its own, or the neutral one while its machine is not reached.
    pub(crate) fn shown_kind(&self, entry: state::Entry<'_>) -> state::Kind {
        match self.as_of(entry.workspace()) {
            Some(_) => state::Kind::Status(qoop_domain::status::Status::Unknown),
            None => entry.kind(),
        }
    }

    /// A row's state word as shown beside [`Self::shown_kind`]'s mark: "Needs you", or
    /// "Needs you as of 15:58" while its machine is not reached.
    pub(crate) fn shown_word(&self, row: &qoop_runtime::control::AgentRow) -> String {
        let word = state::Kind::of(row).word();
        match self.as_of(row.workspace) {
            Some(when) => format!("{word} {when}"),
            None => word.to_string(),
        }
    }

    /// A terminal's screen is the last one seen: the link or its machine is not answering.
    pub(crate) fn term_stale(&self, terminal: TerminalStream) -> bool {
        !self.connected
            || self
                .model
                .row(terminal)
                .is_some_and(|r| self.as_of(r.workspace).is_some())
    }

    /// The banner: each machine not reached, in its sidebar line's words.
    pub(crate) fn banner(&self) -> Option<String> {
        let machines: Vec<(String, state::MachineState)> = state::machines(&self.model.spaces)
            .into_iter()
            .map(|(m, name, _)| (name, self.machine_state(m)))
            .collect();
        state::reach_banner(&machines, self.zone)
    }

    /// A machine's pet for its sidebar line: this Mac's own, another's as its daemon drew it.
    pub(crate) fn pet_of(&self, m: usize) -> Option<gpui::Div> {
        if m == 0 {
            return self.my_pet.as_ref().map(|pet| ui::pet(pet, 0.8));
        }
        self.machine_pets
            .get(&m)
            .filter(|rects| !rects.is_empty())
            .map(|rects| ui::pet_rects(rects, 0.8))
    }

    /// Ask each other machine whose link is up for a word from its daemon, every
    /// [`state::PING_EVERY`]: its pet, which also draws its sidebar line. A link can stay up
    /// while the daemon behind it says nothing; the answers are what [`state::Reach`] counts.
    /// A parked machine is not asked, since asking wakes it.
    fn ping_machines(&mut self) {
        for (m, _, via) in state::machines(&self.model.spaces) {
            let Some(workspace) = via else { continue };
            let up = self.model.spaces.iter().any(|w| {
                state::machine_of(w.id) == m && w.link == Some(qoop_runtime::control::LinkState::Up)
            });
            let due = self
                .pinged
                .get(&m)
                .is_none_or(|at| self.now - at >= state::PING_EVERY);
            // One ask out at a time: a machine that does not answer gets no pile of them.
            let out = self.pet_asks.values().any(|a| *a == m);
            if !up || !due || out {
                continue;
            }
            self.pinged.insert(m, self.now);
            let id = link::fresh_id("pet");
            self.pet_asks.insert(id.clone(), m);
            self.ask(ControlRequest::Via {
                id: link::fresh_id("via"),
                workspace,
                request: Box::new(ControlRequest::Pet { id }),
            });
        }
    }

    /// Try machine `m` now: anything asked of a link that is down makes the mux try it at once
    /// rather than at the end of its backoff (up to 10 s). Its pet is the ask: it costs nothing
    /// and is the word that counts as heard once the machine answers.
    pub(crate) fn retry_machine(&mut self, m: usize) {
        let Some(workspace) = state::machines(&self.model.spaces)
            .into_iter()
            .find(|(n, _, _)| *n == m)
            .and_then(|(_, _, via)| via)
        else {
            return;
        };
        let id = link::fresh_id("pet");
        self.pet_asks.insert(id.clone(), m);
        self.pinged.insert(m, self.now);
        self.ask(ControlRequest::Via {
            id: link::fresh_id("via"),
            workspace,
            request: Box::new(ControlRequest::Pet { id }),
        });
    }

    /// Start This Mac's daemon again (`qoop ensure-awake`, as the TUI's launcher does): the
    /// window connects by itself once it is up.
    pub(crate) fn start_qoop(&mut self, cx: &mut Context<Self>) {
        if self.qoop_said.as_deref() == Some(STARTING_QOOP) {
            return;
        }
        let Some(qoop) = state::qoop_bin() else {
            self.qoop_said = Some(format!(
                "qoop is not installed. Install it with the one-line install: {}",
                state::INSTALL_LINE
            ));
            cx.notify();
            return;
        };
        self.qoop_said = Some(STARTING_QOOP.into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ran = cx
                .background_executor()
                .spawn(async move {
                    std::process::Command::new(qoop)
                        .arg("ensure-awake")
                        .stdin(std::process::Stdio::null())
                        .output()
                })
                .await;
            let said = match ran {
                Ok(out) if out.status.success() => None,
                Ok(out) => Some(
                    String::from_utf8_lossy(&out.stderr)
                        .trim()
                        .trim_start_matches("qoop: ")
                        .to_string(),
                ),
                Err(e) => Some(format!("qoop did not start: {e}")),
            };
            let _ = this.update(cx, |d, cx| {
                // Started: the line stays until the window is connected again.
                if said.is_some() || d.connected {
                    d.qoop_said = said;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The window is in front again: every machine not answering is tried at once, as a
    /// person looking is the moment it should be back.
    pub(crate) fn retry_away(&mut self) {
        let away: Vec<usize> = state::machines(&self.model.spaces)
            .into_iter()
            .map(|(m, _, _)| m)
            .filter(|m| *m != 0 && self.machine_state(*m) != state::MachineState::Live)
            .collect();
        for m in away {
            self.retry_machine(m);
        }
    }

    /// The agent on screen had its question answered by another client: its card stays,
    /// closed. A new question on it takes the card back.
    fn answered_elsewhere(&mut self, before: &[qoop_runtime::control::AgentRow]) {
        let Page::Session(terminal) = self.page else {
            return;
        };
        let Some(now) = self.model.row(terminal) else {
            return;
        };
        if now.menu.is_some() {
            self.closed_card = None;
            return;
        }
        let ours = matches!(
            self.said.get(&state::Slot::Agent(terminal)),
            Some(Said::Sending { .. } | Said::Sent { .. })
        );
        let Some(was) = before.iter().find(|r| r.terminal == terminal) else {
            return;
        };
        if let (Some(line), Some(menu), false) = (
            state::answered_elsewhere(was, now, self.zone),
            was.menu.clone(),
            ours,
        ) {
            self.closed_card = Some((terminal, menu, line));
        }
    }

    /// Once a second: the clock for ages and Next runs, and seen. Draws again only when
    /// something shown can have changed: every second while an age is under a minute, else
    /// every half minute.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = unix_now();
        let fresh = self.model.live().any(|r| {
            r.last_active_at_unix
                .is_some_and(|at| now.saturating_sub(at) < 60)
        });
        let redraw = now != self.now && (fresh || now / 30 != self.now / 30);
        self.now = now;
        self.ping_machines();
        self.keep_beating(window, cx);
        let board_moved = self.board_tick(now);
        let states: Vec<state::MachineState> = state::machines(&self.model.spaces)
            .into_iter()
            .map(|(m, _, _)| self.machine_state(m))
            .collect();
        let redraw = redraw || board_moved || states != self.drawn_states;
        self.drawn_states = states;
        qoop_tui::inbox_pane::day_start(&mut self.day_start, now);
        let redraw = self.follow_config() || redraw;
        let finish = match self.page {
            Page::Session(t) => self.model.finish(t),
            _ => None,
        };
        let present = finish.is_some()
            && seen::present(
                Some(window.is_window_active()),
                self.last_input,
                Instant::now(),
                seen::locked(),
            );
        let finish = finish.map(|(t, stop)| (t, stop.to_string()));
        if seen::step(
            &mut self.look,
            finish.as_ref().map(|(t, s)| (*t, s.as_str())),
            present,
            Instant::now(),
        ) && let Some((terminal, stop)) = finish
        {
            self.ask(ControlRequest::Seen {
                id: link::fresh_id("seen"),
                terminal,
                stop: Some(stop),
            });
        }
        if redraw {
            cx.notify();
        }
    }

    pub(crate) fn title(&self) -> (String, Option<String>) {
        match self.page {
            Page::Inbox => ("Inbox".into(), None),
            Page::Session(t) => {
                let row = self.model.row(t);
                let space = row
                    .and_then(|r| self.model.space(r.workspace))
                    .map(|w| w.name.clone());
                (
                    row.map_or_else(|| format!("tab {t}"), state::title_of),
                    space,
                )
            }
            Page::Qass => ("qass".into(), None),
            Page::Computer(m) => (
                state::machines(&self.model.spaces)
                    .into_iter()
                    .find(|(n, _, _)| *n == m)
                    .map_or_else(|| "Computer".to_string(), |(_, name, _)| name),
                None,
            ),
            Page::Schedules => (
                "Schedules".into(),
                Some(state::schedules_line(&self.model.schedules)),
            ),
            // Another machine's settings: its name, never This Mac's file.
            Page::Settings => (
                "Settings".into(),
                match self.settings.via {
                    Some(via) => self.model.space(via).map(state::machine_name),
                    None => Some(state::tilde(&self.config_path.display().to_string())),
                },
            ),
        }
    }

    /// The page: its header where it has none of its own, and its body, in the main column.
    fn centre(
        &self,
        inbox: &qoop_tui::inbox::Inbox,
        t: &Theme,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body = div()
            .id("centre")
            .role(Role::Main)
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col();
        let padded = |page: AnyElement| div().flex_1().min_h_0().p(px(12.)).child(page);
        match self.page {
            Page::Inbox => body.child(self.inbox_page(inbox, t, cx)).into_any_element(),
            Page::Session(_) => body
                .pt(px(12.))
                .child(padded(self.session(inbox, t, focused, cx)))
                .into_any_element(),
            Page::Qass => body
                .child(self.page_header(t))
                .child(padded(self.qass_page(cx)))
                .into_any_element(),
            Page::Schedules => body
                .child(self.page_header(t))
                .child(padded(self.schedules_page(t, cx)))
                .into_any_element(),
            Page::Settings => body
                .child(self.page_header(t))
                .child(padded(self.settings_page(t, cx)))
                .into_any_element(),
            Page::Computer(m) => {
                ui::scroll(body.overflow_y_scroll().child(self.computer_page(m, t, cx)))
                    .into_any_element()
            }
        }
    }
}

/// Whether to draw dark: the file's theme, or the system's when it says auto.
pub fn dark_for(pref: qoop_tui::config::Theme, appearance: WindowAppearance) -> bool {
    match pref {
        qoop_tui::config::Theme::Dark => true,
        qoop_tui::config::Theme::Light => false,
        qoop_tui::config::Theme::Auto => {
            matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    }
}

/// Text from the input method for the terminal on screen: a dead key's accented letter, an
/// IME's committed word. Ordinary keys never come here; `term_key` sends them.
impl EntityInputHandler for Desk {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.as_ref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.marked = None;
        if let Some(term) = &self.term
            && self.connected
            && !text.is_empty()
        {
            let terminal = term.terminal;
            let _ = self.up.send(Up::Input(terminal, text.as_bytes().to_vec()));
            self.typed_at.insert(terminal, Instant::now());
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_string());
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let term = self.term.as_ref()?;
        let screen = term.screen.as_ref()?;
        let (top, left) = term.viewport.get().map_or((0, 0), |v| (v.row, v.col));
        let row = screen.cursor.0.saturating_sub(usize::from(top));
        let col = screen.cursor.1.saturating_sub(usize::from(left));
        Some(Bounds::new(
            point(
                element.origin.x + self.cells.width * col as f32,
                element.origin.y + self.cells.height * row as f32,
            ),
            size(self.cells.width, self.cells.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for Desk {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        // Every view drawn after this one (the text fields) reads the theme from here.
        if cx.try_global::<Theme>() != Some(&t) {
            cx.set_global(t);
        }
        let focused = self.term_focus.is_focused(window);
        // The widths the dividers set are held to the window as it is now.
        self.win_w = f32::from(window.viewport_size().width);
        self.win_h = f32::from(window.viewport_size().height);
        let inbox = self.inbox();
        self.follow_card_focus(&inbox, window, cx);
        let side = self.side_px();
        let mut root = div()
            .id("desk")
            .role(Role::Application)
            .aria_label("qoop")
            .key_context("Desk")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::capture_key))
            .on_key_down(cx.listener(Self::key))
            .capture_any_mouse_down(
                cx.listener(|this, _, _, _| this.last_input = Some(Instant::now())),
            )
            // A divider follows the mouse while the button is down, wherever it goes.
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                match (this.drag, event.pressed_button) {
                    (Some(edge), Some(MouseButton::Left)) => {
                        this.drag_to(edge, f32::from(event.position.x));
                        cx.notify();
                    }
                    (Some(_), _) => {
                        this.drag = None;
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .relative()
            .size_full()
            .overflow_hidden()
            .flex()
            .bg(c(t.bg))
            .font_family(UI_FONT)
            .text_size(px(13.5))
            .text_color(c(t.ink))
            .when(t.dark, |d| {
                d.child(div().absolute().left_0().right_0().top_0().h(px(300.)).bg(
                    linear_gradient(
                        180.,
                        linear_color_stop(ca(TOP_GLOW), 0.),
                        linear_color_stop(ca(TOP_GLOW & !0xff), 1.),
                    ),
                ))
            })
            .child(self.sidebar(side, &inbox, &t, cx))
            .child(self.divider(Edge::Side, "Resize the sidebar", &t, cx))
            .child(
                div()
                    .id("main")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .when_some(self.banner_strip(&t, cx), |d, banner| d.child(banner))
                    .child(self.centre(&inbox, &t, focused, cx)),
            );
        if self.sheet.is_some() {
            root = root.child(self.sheet_view(&t, cx));
        }
        root
    }
}

/// Open the window on the isolated or real stack the environment names, and run until it
/// closes.
pub fn main() {
    let socket = match qoop_runtime::runtime_dir() {
        Ok(dir) => qoop_runtime::socket_path(&dir),
        Err(e) => {
            eprintln!("qoop-desktop: {e}");
            std::process::exit(1);
        }
    };
    let config = qoop_tui::config::read();
    let (up, mut down, latest) = link::start(socket, (FOREGROUND, BACKGROUND));
    gpui_platform::application()
        .with_assets(ui::Assets)
        .run(move |cx: &mut App| {
            let mut fonts: Vec<Cow<'static, [u8]>> = crate::theme::FONTS
                .iter()
                .map(|f| Cow::Borrowed(*f))
                .collect();
            fonts.push(Cow::Borrowed(qoop_tui::icons::FONT));
            if let Err(e) = cx.text_system().add_fonts(fonts) {
                eprintln!("qoop-desktop: fonts not loaded: {e}");
            }
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1024.), px(680.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("qoop".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(18.))),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let desk =
                        cx.new(|cx| Desk::new(up.clone(), latest.clone(), &config, window, cx));
                    wire(&desk, &mut down, window, cx);
                    desk
                },
            );
            if opened.is_err() {
                eprintln!("qoop-desktop: could not open a window");
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}

/// Feed the link's news to the window, follow the system's appearance, and tick the clock.
fn wire(
    desk: &Entity<Desk>,
    down: &mut tokio::sync::mpsc::UnboundedReceiver<Down>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut down = std::mem::replace(down, tokio::sync::mpsc::unbounded_channel().1);
    let weak = desk.downgrade();
    window
        .spawn(cx, async move |cx| {
            while let Some(first) = down.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = down.try_recv() {
                    batch.push(more);
                }
                let applied = weak.update_in(cx, |this, window, cx| {
                    for d in batch {
                        this.apply(d, window, cx);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        })
        .detach();
    let weak = desk.downgrade();
    window
        .spawn(cx, async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    desk.update(cx, |_, cx| {
        // A pick on hold goes at once when the window closes or the app quits, given a moment
        // for the link to write it.
        cx.on_release(|this: &mut Desk, _| {
            this.flush_holds();
            this.save_held_pet(true);
        })
        .detach();
        cx.on_app_quit(|this: &mut Desk, cx| {
            this.flush_holds();
            this.save_held_pet(true);
            cx.background_executor().timer(Duration::from_millis(200))
        })
        .detach();
        // Back to the window: the settings page reads the file again (it may have been edited).
        cx.observe_window_activation(window, |this: &mut Desk, window, cx| {
            if window.is_window_active() {
                this.retry_away();
            }
            if window.is_window_active() && this.page == Page::Settings {
                this.read_settings();
                cx.notify();
            }
        })
        .detach();
        cx.observe_window_appearance(window, |this: &mut Desk, window, cx| {
            this.appearance = window.appearance();
            this.dark = dark_for(this.theme_pref, this.appearance);
            cx.notify();
        })
        .detach();
    });
}
