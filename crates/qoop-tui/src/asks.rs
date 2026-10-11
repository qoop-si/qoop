//! The question inbox pane: the cards qass keeps for the owner, as a list over the open card.
//!
//! Beside qass's terminal (or in its place, in a narrow pane) the owner reads one card in full
//! and answers it with one key or tap, or in his own words in the one text box. Everything
//! here is a pure function of the cards the daemon sent and of what this client remembers (which
//! card is open, what is typed, the answer being held), so a test reads the pane instead of
//! looking at one.
//!
//! An answer is never sent at once: it is held for [`HOLD`] with the words it would send in
//! front of him, and `u` takes it back. Until the hold ends the card is still open at the
//! daemon, so a quit or a detach in that time drops the answer and loses nothing.

use std::time::{Duration, Instant};

use qoop_runtime::control::{AgentRow, WorkspaceRow};
use qoop_runtime::control::{AskInfo, AskOp, AskResult, ControlRequest, ControlResponse};

use crate::layout::Rect;
use crate::qass_screen::{Seg, bold, edge, ink, line, seg};
use crate::sidebar;
use crate::theme::{self, Role};
use crate::width;

/// How long an answer is held, with the words it sends in front of him, before it goes.
pub const HOLD: Duration = Duration::from_secs(5);

/// How long a word about what just happened stands in place of the key line.
const NOTICE: Duration = Duration::from_secs(6);

/// A card this old is shown dim; it is never closed for being old.
pub const OLD_SECS: i64 = 24 * 3600;

/// The id of the request that reads the inbox, and the prefix of the ones that write it.
pub const READ_ID: &str = "asks-read";
pub const WRITE_ID: &str = "asks-write";

/// The most characters of his own words one answer carries (the daemon's own limit).
const TEXT_MAX: usize = 400;

/// What a held answer will do when the hold ends.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Answer {
        option: Option<String>,
        text: Option<String>,
        /// The words of the option as he saw them.
        seen: Option<String>,
    },
    Dismiss,
}

/// An answer or a dismissal waiting out its hold.
#[derive(Clone, Debug)]
pub struct Hold {
    card: String,
    /// What he would type in the chat to say the same: `korea B`, or `korea` and his words.
    typed: String,
    /// What the banner says it sends.
    words: String,
    held: Held,
    until: Instant,
}

/// What a key did, for the loop that owns the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Taken: repaint.
    Stay,
    /// The keyboard goes back to qass's chat, the inbox stays open.
    ToChat,
    /// The inbox is put away and the keyboard goes back to the chat.
    Closed,
    /// Go to this agent's tab.
    Jump(u32),
    /// Not the inbox's: the prefix, a mouse report or a detach, for the ordinary path.
    Pass(Vec<u8>),
}

/// The inbox as this client keeps it.
#[derive(Debug, Default)]
pub struct Pane {
    /// The keyboard is in the inbox rather than in qass's chat.
    pub focus: bool,
    /// The cards waiting for him (`open`) or being relayed (`passed`), oldest first.
    pub live: Vec<AskInfo>,
    /// The last few that ended, newest first.
    pub ended: Vec<AskInfo>,
    /// The daemon has answered a read once: the chip counts cards, not blocked agents.
    pub loaded: bool,
    /// The daemon predates the inbox.
    pub unsupported: bool,
    /// The read is owed: a change event came, or the first read has not been asked.
    pub stale: bool,
    selected: usize,
    /// The option whose words show under it, by position in the open card.
    preview: Option<usize>,
    typing: bool,
    text: String,
    hold: Option<Hold>,
    /// What the daemon last said about an answer: shown in place of the key line.
    notice: Option<String>,
    /// When the notice is let go.
    notice_until: Option<Instant>,
    /// An answer sent and not yet answered, by its request id.
    sent: Option<(String, String)>,
    /// The second the countdown was last painted for.
    shown_left: Option<u64>,
    /// Cards already read again because their terminal left the rows.
    gone: Vec<String>,
    /// The open cards of the last read, each with a hash of its body: what tells a new
    /// question (newly open, reopened or rewritten, all with the same `opened_at`) from one
    /// already there.
    last_open: std::collections::BTreeMap<String, u64>,
    /// The owner put the questions away; the next new question brings them back.
    pub hidden: bool,
}

impl Pane {
    /// An inbox not yet read: the first read is owed.
    pub fn new() -> Pane {
        Pane {
            stale: true,
            ..Pane::default()
        }
    }

    /// The cards the chip counts: those waiting for him.
    pub fn waiting(&self) -> usize {
        self.live.iter().filter(|c| c.status == "open").count()
    }

    /// The count for the chip: cards once readable, `None` before (blocked agents then).
    pub fn count(&self) -> Option<usize> {
        (self.loaded && !self.unsupported).then(|| self.waiting())
    }

    /// The open card, if any.
    pub fn open_card(&self) -> Option<&AskInfo> {
        self.live
            .get(self.selected.min(self.live.len().saturating_sub(1)))
    }

    /// Take a fresh read: the open card stays the open card when it is still there.
    pub fn take(&mut self, live: Vec<AskInfo>, ended: Vec<AskInfo>) {
        let open: std::collections::BTreeMap<String, u64> = live
            .iter()
            .filter(|c| c.status == "open")
            .map(|c| (c.card.clone(), body_hash(c)))
            .collect();
        if open
            .iter()
            .any(|(card, hash)| self.last_open.get(card) != Some(hash))
        {
            self.hidden = false;
        }
        self.last_open = open;
        let keep = self.open_card().map(|c| c.card.clone());
        self.live = live;
        self.ended = ended;
        self.loaded = true;
        self.unsupported = false;
        self.selected = keep
            .as_ref()
            .and_then(|k| self.live.iter().position(|c| c.card == *k))
            .unwrap_or(self.selected)
            .min(self.live.len().saturating_sub(1));
        self.preview = None;
        // Words typed for a card are not for the one that took its place.
        if self.open_card().map(|c| &c.card) != keep.as_ref() {
            self.typing = false;
            self.text.clear();
        }
        // A hold for a card that is gone is a hold for nothing.
        if let Some(h) = &self.hold
            && !self.live.iter().any(|c| c.card == h.card)
        {
            self.hold = None;
        }
    }

    /// A card whose terminal has left the agent rows (and sleeps in no tab) is read again, once:
    /// the daemon closed it with the terminal and sends no event for that.
    pub fn note_gone(&mut self, rows: &[AgentRow], spaces: &[WorkspaceRow]) {
        // Before the first index there is nobody to compare with.
        if !self.loaded || rows.is_empty() {
            return;
        }
        let here = |t: u32| {
            rows.iter().any(|r| r.terminal == t)
                || spaces
                    .iter()
                    .any(|w| w.sleeping.iter().any(|s| s.terminal == t))
        };
        // A terminal that is back (a wake) may leave again: it is watched afresh.
        let live = &self.live;
        self.gone.retain(|card| {
            !live
                .iter()
                .any(|c| c.card == *card && c.terminal.is_some_and(here))
        });
        for c in &self.live {
            let Some(t) = c.terminal else { continue };
            if !here(t) && !self.gone.contains(&c.card) {
                self.gone.push(c.card.clone());
                self.stale = true;
            }
        }
    }

    /// The read to send, when one is owed and none is in flight.
    pub fn read_request(&mut self) -> Option<ControlRequest> {
        if !self.stale {
            return None;
        }
        self.stale = false;
        Some(ControlRequest::AskRead {
            id: READ_ID.to_string(),
            all: false,
        })
    }

    /// The daemon's answer to a request of the inbox's. True when the screen changed.
    pub fn answered(&mut self, response: &ControlResponse) -> bool {
        match response {
            ControlResponse::Asks { id, live, ended } if id == READ_ID => {
                self.take(live.clone(), ended.clone());
                true
            }
            ControlResponse::AskWritten { id, result, .. } if self.is_mine(id) => {
                let typed = self.sent.take().map(|(_, typed)| typed).unwrap_or_default();
                let said = match result {
                    AskResult::Answered => "sent".to_string(),
                    AskResult::Passed => "passed to qass".to_string(),
                    AskResult::Busy => format!("qass is busy, type {typed} in the chat"),
                    AskResult::Closed => "put away".to_string(),
                    _ => "done".to_string(),
                };
                self.say(said);
                self.stale = true;
                true
            }
            ControlResponse::Failed { id, reason } if self.is_mine(id) => {
                self.sent = None;
                self.say(reason.clone());
                self.stale = true;
                true
            }
            ControlResponse::Failed { id, reason }
                if id == READ_ID
                    || (id.is_empty()
                        && qoop_runtime::control::too_old_for(reason, Some("AskRead"))) =>
            {
                self.unsupported = true;
                self.loaded = false;
                true
            }
            _ => false,
        }
    }

    fn is_mine(&self, id: &str) -> bool {
        self.sent.as_ref().is_some_and(|(sent, _)| sent == id)
    }

    /// The moment the inbox next has something to do by itself: a hold running out or a
    /// countdown second to repaint.
    pub fn wake_at(&self) -> Option<Instant> {
        let hold = self
            .hold
            .as_ref()
            .map(|h| h.until.min(Instant::now() + Duration::from_millis(250)));
        match (hold, self.notice_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Say something in place of the key line, for a few seconds.
    fn say(&mut self, what: impl Into<String>) {
        self.notice = Some(what.into());
        self.notice_until = Some(Instant::now() + NOTICE);
    }

    /// A tick: the request that sends a held answer whose time is up, and whether the
    /// countdown changed.
    pub fn tick(&mut self, now: Instant) -> (Option<ControlRequest>, bool) {
        let mut moved = false;
        if self.notice_until.is_some_and(|until| now >= until) {
            self.notice = None;
            self.notice_until = None;
            self.notice_until = None;
            moved = true;
        }
        let Some(hold) = &self.hold else {
            return (None, moved);
        };
        if now < hold.until {
            let left = left_secs(hold.until, now);
            moved |= self.shown_left != Some(left);
            self.shown_left = Some(left);
            return (None, moved);
        }
        let hold = self.hold.take().expect("checked above");
        self.shown_left = None;
        self.notice = None;
        self.notice_until = None;
        let id = format!("{WRITE_ID}-{}", hold.card);
        self.sent = Some((id.clone(), hold.typed.clone()));
        let action = match hold.held {
            Held::Answer { option, text, seen } => AskOp::Answer {
                card: hold.card,
                option,
                text,
                seen,
            },
            Held::Dismiss => AskOp::Dismiss { card: hold.card },
        };
        (
            Some(ControlRequest::AskWrite {
                id,
                terminal: None,
                token: None,
                action,
            }),
            true,
        )
    }

    /// The keyboard leaves the inbox: an answer still being held is taken back, because
    /// nothing on screen would let him see it go or stop it.
    pub fn release(&mut self) {
        self.focus = false;
        self.typing = false;
        if self.hold.take().is_some() {
            self.shown_left = None;
            self.say("taken back");
        }
    }

    /// Whether an answer is being held, for the key line.
    pub fn holding(&self) -> bool {
        self.hold.is_some()
    }

    /// Whether an answer is held, on its way or just answered: the questions stay up through
    /// all three, so the last answer's word is still seen.
    pub fn busy(&self) -> bool {
        self.hold.is_some() || self.sent.is_some() || self.notice.is_some()
    }

    /// Whether the questions show by themselves: a card waits and the owner has not put them
    /// away since it came.
    pub fn calling(&self) -> bool {
        self.waiting() > 0 && !self.hidden
    }

    fn hold_answer(&mut self, option: Option<usize>, text: Option<String>, now: Instant) -> bool {
        let Some(card) = self.open_card() else {
            return false;
        };
        if card.status != "open" {
            return false;
        }
        let (words, held) = match (option.and_then(|i| card.body.options.get(i)), text) {
            (_, Some(text)) => (
                text.clone(),
                Held::Answer {
                    option: None,
                    text: Some(text),
                    seen: None,
                },
            ),
            (Some(o), None) => (
                o.words.clone(),
                Held::Answer {
                    option: Some(o.key.clone()),
                    text: None,
                    seen: Some(o.words.clone()),
                },
            ),
            (None, None) => return false,
        };
        let typed = match &held {
            Held::Answer {
                option: Some(k), ..
            } => format!("{} {}", card.body.handle, k.to_uppercase()),
            Held::Answer { text, .. } => {
                format!(
                    "{} {}",
                    card.body.handle,
                    text.as_deref().unwrap_or_default()
                )
            }
            Held::Dismiss => String::new(),
        };
        self.hold = Some(Hold {
            card: card.card.clone(),
            typed,
            words,
            held,
            until: now + HOLD,
        });
        self.notice = None;
        self.notice_until = None;
        self.shown_left = None;
        self.typing = false;
        self.text.clear();
        true
    }

    fn hold_dismiss(&mut self, now: Instant) -> bool {
        let Some(card) = self.open_card() else {
            return false;
        };
        self.hold = Some(Hold {
            card: card.card.clone(),
            typed: String::new(),
            words: "put away unanswered".to_string(),
            held: Held::Dismiss,
            until: now + HOLD,
        });
        self.notice = None;
        self.notice_until = None;
        self.shown_left = None;
        true
    }

    /// Answer with option `at` of the open card, held first.
    pub fn press_option(&mut self, at: usize, now: Instant) -> bool {
        self.hold_answer(Some(at), None, now)
    }

    /// Open card number `at` of the list.
    pub fn select(&mut self, at: usize) {
        if at < self.live.len() {
            self.selected = at;
            self.preview = None;
            self.typing = false;
            self.text.clear();
            self.notice = None;
            self.notice_until = None;
        }
    }

    /// Start typing in the one text box.
    pub fn start_typing(&mut self) {
        if self.open_card().is_some_and(|c| c.status == "open") {
            self.typing = true;
            self.notice = None;
            self.notice_until = None;
        }
    }

    /// Move the open card by one.
    fn step(&mut self, down: bool) {
        let last = self.live.len().saturating_sub(1);
        let at = self.selected.min(last);
        let to = if down {
            (at + 1).min(last)
        } else {
            at.saturating_sub(1)
        };
        self.select(to);
    }

    /// Move the previewed option by one.
    fn step_option(&mut self, down: bool) {
        let Some(card) = self.open_card() else {
            return;
        };
        let n = card.body.options.len();
        if n == 0 {
            return;
        }
        let now = self.preview_at(card);
        self.preview = Some(if down {
            (now + 1) % n
        } else {
            (now + n - 1) % n
        });
    }

    /// The option whose words show: the one he moved to, else qass's pick, else the first.
    fn preview_at(&self, card: &AskInfo) -> usize {
        self.preview
            .filter(|i| *i < card.body.options.len())
            .or_else(|| {
                let pick = card.body.pick.as_ref()?;
                card.body.options.iter().position(|o| o.key == pick.key)
            })
            .unwrap_or(0)
    }

    /// A key, or the bytes of several. `prefix` is the interface's prefix byte, which with a
    /// mouse report or a detach is not the inbox's and goes back.
    pub fn key(&mut self, bytes: &[u8], prefix: u8, now: Instant) -> Out {
        if bytes.first() == Some(&prefix) || bytes.starts_with(b"\x1b[<") || bytes == b"\x11" {
            return Out::Pass(bytes.to_vec());
        }
        if self.typing {
            return self.type_key(bytes, now);
        }
        let mut out = Out::Stay;
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            let (key, used) = if rest.starts_with(b"\x1b[A") || rest.starts_with(b"\x1bOA") {
                (Key::Up, 3)
            } else if rest.starts_with(b"\x1b[B") || rest.starts_with(b"\x1bOB") {
                (Key::Down, 3)
            } else if rest[0] == 0x1b && rest.len() == 1 {
                (Key::Esc, 1)
            } else if rest[0] == 0x1b {
                // Some other sequence: not ours, and not worth reading as a letter.
                return Out::Stay;
            } else {
                (Key::Char(rest[0]), 1)
            };
            i += used;
            match self.act(key, now) {
                Out::Stay => {}
                other => {
                    out = other;
                    break;
                }
            }
        }
        out
    }

    fn act(&mut self, key: Key, now: Instant) -> Out {
        match key {
            Key::Esc => Out::Closed,
            Key::Up => {
                self.step_option(false);
                Out::Stay
            }
            Key::Down => {
                self.step_option(true);
                Out::Stay
            }
            Key::Char(b'\t') => Out::ToChat,
            Key::Char(b'j') => {
                self.step(true);
                Out::Stay
            }
            Key::Char(b'k') => {
                self.step(false);
                Out::Stay
            }
            Key::Char(c @ b'a'..=b'd') => {
                self.press_option((c - b'a') as usize, now);
                Out::Stay
            }
            Key::Char(b't') => {
                self.start_typing();
                Out::Stay
            }
            Key::Char(b'x') => {
                self.hold_dismiss(now);
                Out::Stay
            }
            Key::Char(b'u') => {
                if self.hold.take().is_some() {
                    self.shown_left = None;
                    self.say("taken back");
                }
                Out::Stay
            }
            Key::Char(b'\r' | b'\n') => match self.open_card().and_then(|c| c.terminal) {
                Some(terminal) => Out::Jump(terminal),
                None => Out::Stay,
            },
            Key::Char(_) => Out::Stay,
        }
    }

    fn type_key(&mut self, bytes: &[u8], now: Instant) -> Out {
        if bytes == b"\x1b" {
            self.typing = false;
            return Out::Stay;
        }
        // An arrow, a delete or a paste's brackets are not letters.
        if bytes.len() > 1 && bytes[0] == 0x1b {
            return Out::Stay;
        }
        // ctrl-u clears the line.
        if bytes == b"\x15" {
            self.text.clear();
            return Out::Stay;
        }
        for c in String::from_utf8_lossy(bytes).chars() {
            match c {
                '\r' | '\n' => {
                    let text = self.text.trim().to_string();
                    if !text.is_empty() {
                        self.hold_answer(None, Some(text), now);
                    }
                    return Out::Stay;
                }
                '\u{7f}' | '\u{8}' => {
                    self.text.pop();
                }
                c if c.is_control() => {}
                c => {
                    if self.text.chars().count() < TEXT_MAX {
                        self.text.push(c);
                    }
                }
            }
        }
        Out::Stay
    }

    /// A click on something the view drew.
    pub fn click(&mut self, hit: Hit, now: Instant) -> Out {
        match hit {
            Hit::Card(at) => self.select(at),
            Hit::Option(at) => {
                self.press_option(at, now);
            }
            Hit::Box => self.start_typing(),
            Hit::Board => {
                self.say("the board is not here yet");
            }
        }
        Out::Stay
    }

    /// His own words so far.
    pub fn typed(&self) -> &str {
        &self.text
    }

    pub fn typing(&self) -> bool {
        self.typing
    }
}

enum Key {
    Esc,
    Up,
    Down,
    Char(u8),
}

/// What a click in the inbox landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A card in the list, by position.
    Card(usize),
    /// An option of the open card, by position.
    Option(usize),
    /// The text box.
    Box,
    /// The second tab.
    Board,
}

/// A cell range on one row of the inbox that a click answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub row: usize,
    pub x0: usize,
    pub x1: usize,
    pub hit: Hit,
}

/// What painting needs besides the pane: the rows to name agents by and the clocks.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub rows: &'a [AgentRow],
    pub spaces: &'a [WorkspaceRow],
    pub now_unix: i64,
    pub now: Instant,
    /// A box is open over the screen, which fades everything behind it.
    pub fade: bool,
    /// The board is a pane of qass's tab: the inbox keeps no tab for it.
    pub board_above: bool,
}

/// The inbox painted: one string per row, each exactly as wide as asked, and where a click
/// lands.
#[derive(Clone, Debug)]
pub struct View {
    pub rows: Vec<String>,
    pub targets: Vec<Target>,
    /// Where typing goes, relative to the inbox: its row and column, while the box has the keys.
    pub cursor: Option<(usize, usize)>,
}

/// Whole seconds left of a hold, counted up: five at the start, one at the end.
fn left_secs(until: Instant, now: Instant) -> u64 {
    until.saturating_duration_since(now).as_secs_f64().ceil() as u64
}

fn age_short(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "now".to_string(),
        s @ 60..=3599 => format!("{}m", s / 60),
        s @ 3600..=259_199 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn age_long(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "under a minute".to_string(),
        s @ 60..=3599 => format!("{} min", s / 60),
        s @ 3600..=259_199 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// The name he knows an agent by: its row's, else the handle qass wrote on the card.
fn name_of(card: &AskInfo, ctx: &Ctx) -> String {
    card.terminal
        .and_then(|t| ctx.rows.iter().find(|r| r.terminal == t))
        .map(sidebar::session_name)
        .unwrap_or_else(|| card.body.handle.clone())
}

fn space_of(card: &AskInfo, ctx: &Ctx) -> Option<String> {
    let row = card
        .terminal
        .and_then(|t| ctx.rows.iter().find(|r| r.terminal == t))?;
    ctx.spaces
        .iter()
        .find(|w| w.id == row.workspace)
        .map(|w| w.name.clone())
}

/// A tag on the open card that says why it may not be what it was, from the rows alone.
fn tag_of(card: &AskInfo, ctx: &Ctx) -> Option<String> {
    if card.status == "passed" {
        let ago = card
            .closed_at
            .map(|at| format!(", {} ago", age_long(ctx.now_unix - at)))
            .unwrap_or_default();
        return Some(format!("passed to qass{ago}"));
    }
    let terminal = card.terminal?;
    if ctx
        .spaces
        .iter()
        .any(|w| w.sleeping.iter().any(|t| t.terminal == terminal))
    {
        return Some("asleep".to_string());
    }
    let row = ctx.rows.iter().find(|r| r.terminal == terminal)?;
    (row.state == "working").then(|| "working again".to_string())
}

/// How the pane is divided between the parts, given its height.
struct Budget {
    list: usize,
    asked: bool,
    why: bool,
    gap_top: bool,
    gap_box: bool,
    pick: bool,
    sends: bool,
    doing: bool,
    handled: usize,
}

fn budget(
    inner_rows: usize,
    cards: usize,
    options: usize,
    has_asked: bool,
    has_pick: bool,
) -> Budget {
    let mut b = Budget {
        list: cards.clamp(1, 5),
        asked: has_asked,
        why: true,
        gap_top: true,
        gap_box: true,
        pick: has_pick,
        sends: true,
        doing: true,
        handled: 5,
    };
    let need = |b: &Budget| {
        // tabs, rule, list, rule, hints, and the detail below.
        let detail = 2
            + 1
            + usize::from(b.asked)
            + usize::from(b.doing)
            + usize::from(b.why)
            + usize::from(b.gap_top)
            + options
            + usize::from(b.sends)
            + usize::from(b.pick)
            + usize::from(b.gap_box)
            + 3;
        let handled = if b.handled > 0 { b.handled + 2 } else { 0 };
        2 + b.list + 1 + detail + handled + 1
    };
    while need(&b) > inner_rows {
        if b.handled > 0 {
            b.handled = b.handled.saturating_sub(1);
            if b.handled < 2 {
                b.handled = 0;
            }
        } else if b.list > 3 {
            b.list -= 1;
        } else if b.gap_top {
            b.gap_top = false;
        } else if b.asked {
            b.asked = false;
        } else if b.gap_box {
            b.gap_box = false;
        } else if b.why {
            b.why = false;
        } else if b.pick {
            b.pick = false;
        } else if b.list > 1 {
            b.list -= 1;
        } else if b.sends {
            b.sends = false;
        } else if b.doing {
            b.doing = false;
        } else {
            break;
        }
    }
    b
}

/// The inbox's rows for a box `cols x rows` (border included).
pub fn view(pane: &Pane, ctx: &Ctx, cols: usize, rows: usize) -> View {
    let fade = ctx.fade;
    let cw = cols.saturating_sub(4);
    let inner_rows = rows.saturating_sub(2);
    let mut out: Vec<String> = Vec::new();
    let mut targets = Vec::new();
    let mut cursor = None;

    // One content row: a margin, the pieces padded to `cw`, a margin, between the box's sides.
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
    let hr = || wrap(&[seg(Role::Rule, "\u{2500}".repeat(cw))]);

    out.push(edge(cols, ('\u{256d}', '\u{256e}'), None, None, fade));

    // The tabs.
    let n = pane.waiting();
    let mut tabs = vec![bold(Role::Text, "questions")];
    if n > 0 {
        tabs.push(seg(Role::Text, "  "));
        tabs.push(bold(Role::Blocked, n.to_string()));
    }
    let board_x = width::width("questions") + if n > 0 { 2 + n.to_string().len() } else { 0 } + 2;
    if !ctx.board_above {
        tabs.push(seg(Role::Text, "  "));
        tabs.push(seg(Role::Faint, "board"));
        targets.push(Target {
            row: out.len(),
            x0: 2 + board_x,
            x1: 2 + board_x + 5,
            hit: Hit::Board,
        });
    }
    out.push(wrap(&tabs));
    out.push(hr());

    if pane.unsupported {
        out.push(wrap(&[seg(Role::Detail, "restart qoop for the inbox")]));
    } else if !pane.loaded {
        out.push(wrap(&[seg(Role::Faint, "reading the inbox")]));
    } else if pane.live.is_empty() {
        out.push(blank());
        out.push(wrap(&[seg(Role::Detail, "Nothing waits for you.")]));
    }

    if let Some(card) = pane.open_card() {
        let sel = pane.selected.min(pane.live.len() - 1);
        let b = budget(
            inner_rows,
            pane.live.len(),
            card.body.options.len(),
            card.body.asked.is_some(),
            card.body.pick.is_some(),
        );
        // The list: a window of it that holds the open card.
        let first = sel
            .saturating_sub(b.list.saturating_sub(1))
            .min(pane.live.len().saturating_sub(b.list));
        for (at, c) in pane.live.iter().enumerate().skip(first).take(b.list) {
            let old = ctx.now_unix - c.opened_at >= OLD_SECS;
            let calm = old || c.status == "passed";
            let role = if calm { Role::Faint } else { Role::Text };
            let age = age_short(ctx.now_unix - c.opened_at);
            let name = sidebar::chip_name(&name_of(c, ctx));
            let name_w = 12usize.min(cw / 4);
            let head = format!("{} {}", at + 1, width::fit(&name, name_w));
            let room = cw.saturating_sub(width::width(&head) + 2 + age.len() + 1);
            let asks = cut_dots(&c.body.asks, room);
            let gap = cw.saturating_sub(width::width(&head) + 2 + width::width(&asks) + age.len());
            let mut segs = vec![
                seg(role, head),
                seg(role, "  "),
                seg(if calm { Role::Faint } else { Role::Detail }, asks),
                seg(Role::Text, " ".repeat(gap)),
                seg(Role::Faint, age),
            ];
            if at == sel {
                for s in &mut segs {
                    s.role = if calm { Role::Detail } else { Role::Focused };
                }
                segs[0].bold = true;
            }
            targets.push(Target {
                row: out.len(),
                x0: 2,
                x1: 2 + cw,
                hit: Hit::Card(at),
            });
            out.push(wrap(&segs));
        }
        out.push(hr());

        // The open card.
        let waited = age_long(ctx.now_unix - card.opened_at);
        let space = space_of(card, ctx)
            .map(|s| format!("{s} \u{b7} "))
            .unwrap_or_default();
        let right = format!("{space}waiting {waited}");
        let name = name_of(card, ctx);
        let left_w = cw.saturating_sub(width::width(&right) + 2);
        let title_left = format!("\u{25be} {}", width::fit(&name, left_w.min(24)).trim_end());
        let gap = cw.saturating_sub(width::width(&title_left) + width::width(&right));
        out.push(wrap(&[
            bold(Role::Text, title_left),
            seg(Role::Text, " ".repeat(gap)),
            seg(Role::Faint, right),
        ]));
        out.push(match tag_of(card, ctx) {
            Some(tag) => wrap(&[seg(Role::Blocked, tag)]),
            None => blank(),
        });
        let label = |what: &str, text: &str| {
            wrap(&[
                seg(Role::Faint, format!("{what:<8}")),
                seg(Role::Detail, cut_dots(text, cw.saturating_sub(8))),
            ])
        };
        if b.asked
            && let Some(asked) = &card.body.asked
        {
            out.push(label("asked", asked));
        }
        if b.doing {
            out.push(label("doing", &card.body.doing));
        }
        if b.why {
            out.push(label("why now", &card.body.why));
        }
        if b.gap_top {
            out.push(blank());
        }
        out.push(wrap(&[bold(Role::Text, cut_dots(&card.body.asks, cw))]));
        let passed = card.status != "open";
        let shown = pane.preview_at(card);
        let label_w = card
            .body
            .options
            .iter()
            .map(|o| width::width(&o.label))
            .max()
            .unwrap_or(0)
            .min(cw / 3);
        for (i, o) in card.body.options.iter().enumerate() {
            let held = pane.hold.as_ref().is_some_and(|h| {
                h.card == card.card
                    && matches!(&h.held, Held::Answer { option: Some(k), .. } if *k == o.key)
            });
            let key_ink = if passed { Role::Faint } else { Role::Brand };
            let lab = width::fit(&o.label, label_w);
            let reason_room = cw.saturating_sub(4 + label_w + 2);
            targets.push(Target {
                row: out.len(),
                x0: 2,
                x1: 2 + cw,
                hit: Hit::Option(i),
            });
            out.push(wrap(&[
                seg(key_ink, format!("[{}] ", o.key)),
                if held {
                    bold(Role::Working, lab)
                } else {
                    seg(if passed { Role::Faint } else { Role::Text }, lab)
                },
                seg(Role::Text, "  "),
                seg(Role::Detail, cut_dots(&o.reason, reason_room)),
            ]));
            if b.sends && i == shown {
                out.push(wrap(&[
                    seg(Role::Text, "    "),
                    seg(Role::Faint, "sends: "),
                    seg(Role::Detail, cut_dots(&o.words, cw.saturating_sub(11))),
                ]));
            }
        }
        if b.pick
            && let Some(p) = &card.body.pick
        {
            out.push(wrap(&[
                seg(Role::Brand, format!("qass picks {}", p.key)),
                seg(
                    Role::Detail,
                    format!(": {}", cut_dots(&p.reason, cw.saturating_sub(14))),
                ),
            ]));
        }
        if b.gap_box {
            out.push(blank());
        }
        // The one text box.
        let box_w = cw;
        let title = " type a different answer ";
        let mut top = format!("\u{256d}\u{2500}{title}");
        top.push_str(&"\u{2500}".repeat(box_w.saturating_sub(width::width(&top) + 1)));
        top.push('\u{256e}');
        let box_ink = if pane.typing {
            Role::Focused
        } else {
            Role::Rule
        };
        out.push(wrap(&[seg(box_ink, top)]));
        let inside = box_w.saturating_sub(4);
        let shown_text = if pane.typing {
            // The tail of what is typed, with room for the caret.
            let t = &pane.text;
            let skip = t.chars().count().saturating_sub(inside.saturating_sub(1));
            let tail: String = t.chars().skip(skip).collect();
            format!("{tail}\u{2588}")
        } else if passed {
            "waiting for qass".to_string()
        } else {
            "t to type".to_string()
        };
        targets.push(Target {
            row: out.len(),
            x0: 2,
            x1: 2 + cw,
            hit: Hit::Box,
        });
        let text_role = if pane.typing { Role::Text } else { Role::Faint };
        if pane.typing {
            cursor = Some((
                out.len(),
                2 + 2 + width::width(&shown_text).saturating_sub(1),
            ));
        }
        out.push(wrap(&[
            seg(box_ink, "\u{2502} "),
            seg(text_role, width::fit(&shown_text, inside)),
            seg(box_ink, " \u{2502}"),
        ]));
        out.push(wrap(&[seg(
            box_ink,
            format!(
                "\u{2570}{}\u{256f}",
                "\u{2500}".repeat(box_w.saturating_sub(2))
            ),
        )]));

        // What he already handled.
        if b.handled > 0 && !pane.ended.is_empty() {
            out.push(blank());
            out.push(wrap(&[
                seg(Role::Faint, "handled for you "),
                seg(Role::Detail, pane.ended.len().to_string()),
            ]));
            out.push(hr());
            for c in pane.ended.iter().take(b.handled.saturating_sub(2)) {
                out.push(handled_row(c, ctx, cw, &wrap));
            }
        }
    } else if pane.loaded && !pane.unsupported {
        // Nothing waits: only what was handled.
        out.push(blank());
        if !pane.ended.is_empty() {
            out.push(wrap(&[
                seg(Role::Faint, "handled for you "),
                seg(Role::Detail, pane.ended.len().to_string()),
            ]));
            out.push(hr());
            for c in pane
                .ended
                .iter()
                .take(inner_rows.saturating_sub(out.len() + 2))
            {
                out.push(handled_row(c, ctx, cw, &wrap));
            }
        }
    }

    // The key line closes the content: what a held answer is about to do, what the daemon last
    // said, or the keys.
    let content = inner_rows.saturating_sub(1);
    out.truncate(1 + content);
    while out.len() < 1 + content {
        out.push(blank());
    }
    out.push(wrap(&bottom_line(pane, ctx, cw)));
    out.push(edge(cols, ('\u{2570}', '\u{256f}'), None, None, fade));
    out.truncate(rows);
    // A target below the clipped rows is no target.
    targets.retain(|t| t.row <= content);
    View {
        rows: out,
        targets,
        cursor: cursor.filter(|(r, _)| *r < rows),
    }
}

fn handled_row(c: &AskInfo, ctx: &Ctx, cw: usize, wrap: &impl Fn(&[Seg]) -> String) -> String {
    let result = match (c.status.as_str(), c.note.as_deref()) {
        ("answered", _) => match &c.option {
            Some(k) => format!("you chose {k}"),
            None => "you answered".to_string(),
        },
        (_, Some(note)) => match note {
            "chat" => "answered in chat".to_string(),
            "qass" => "qass answered".to_string(),
            "gone" => "agent gone".to_string(),
            other => other.to_string(),
        },
        _ => "closed".to_string(),
    };
    let age = age_short(ctx.now_unix - c.closed_at.unwrap_or(c.opened_at));
    let name = width::fit(&sidebar::chip_name(&name_of(c, ctx)), 12.min(cw / 4));
    let right = format!("{result}  {age}");
    let room = cw.saturating_sub(width::width(&name) + 2 + width::width(&right) + 1);
    let asks = cut_dots(&c.body.asks, room);
    let gap =
        cw.saturating_sub(width::width(&name) + 2 + width::width(&asks) + width::width(&right));
    wrap(&[
        seg(Role::Faint, name),
        seg(Role::Faint, "  "),
        seg(Role::Faint, asks),
        seg(Role::Text, " ".repeat(gap)),
        seg(Role::Faint, right),
    ])
}

fn bottom_line(pane: &Pane, ctx: &Ctx, cw: usize) -> Vec<Seg> {
    if let Some(h) = &pane.hold {
        let left = left_secs(h.until, ctx.now);
        // Who it goes to, by the name the list shows.
        let to = pane
            .live
            .iter()
            .find(|c| c.card == h.card)
            .map(|c| name_of(c, ctx))
            .unwrap_or_default();
        let verb = match h.held {
            Held::Dismiss => format!("putting {to} away in {left} s"),
            Held::Answer { .. } => format!("sending to {to} in {left} s: {}", h.words),
        };
        let undo = "  u undo";
        return vec![
            seg(
                Role::Working,
                cut_dots(&verb, cw.saturating_sub(undo.len())),
            ),
            bold(Role::Brand, undo),
        ];
    }
    if let Some(n) = &pane.notice {
        return vec![seg(Role::Detail, cut_dots(n, cw))];
    }
    let keys = if pane.typing {
        "enter send  esc stop typing"
    } else if pane.focus {
        "j k cards  a-d send  t type  x away  tab chat"
    } else {
        "ctrl-a i focuses this"
    };
    vec![seg(Role::Faint, cut_dots(keys, cw))]
}

/// `text` cut to `cols` columns with a closing `\u{2026}` when it did not fit.
fn cut_dots(text: &str, cols: usize) -> String {
    if width::width(text) <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| width::drawn(*c)) {
        let w = width::columns(c);
        if used + w + 1 > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    out
}

/// The inbox's rectangle within the pane, for a click: its place and what was painted there.
pub fn hit_at(view: &View, rect: Rect, row: usize, col: usize) -> Option<Hit> {
    let (r, x) = (row.checked_sub(rect.y)?, col.checked_sub(rect.x)?);
    view.targets
        .iter()
        .find(|t| t.row == r && x >= t.x0 && x < t.x1)
        .map(|t| t.hit)
}

/// A card's body as one number, from its JSON: a rewritten question hashes differently.
fn body_hash(card: &AskInfo) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&card.body)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::control::{AskCard, AskOption, AskPick};

    fn card(n: u32, handle: &str, status: &str) -> AskInfo {
        AskInfo {
            card: format!("E{n}"),
            agent: Some(format!("T{n}")),
            terminal: Some(n),
            status: status.to_string(),
            note: None,
            opened_at: 1_000 - 360,
            closed_at: None,
            answer: None,
            option: None,
            task: None,
            body: AskCard {
                handle: handle.to_string(),
                urgency: "later".to_string(),
                asked: Some("drop users.legacy_id".to_string()),
                doing: "wrote migration 0042, dry run on the copy is clean".to_string(),
                why: "the PR cannot open before 0042 has run once on staging".to_string(),
                asks: "Run 0042 on staging now?".to_string(),
                options: vec![
                    AskOption {
                        key: "a".into(),
                        label: "run now".into(),
                        reason: "no rollback exists, staging is a copy".into(),
                        words: "Run 0042 on staging now, then report the row counts.".into(),
                    },
                    AskOption {
                        key: "b".into(),
                        label: "wait for you".into(),
                        reason: "you may want to read the SQL first".into(),
                        words: "Do not run 0042 yet; stay idle and wait for the owner.".into(),
                    },
                    AskOption {
                        key: "c".into(),
                        label: "skip".into(),
                        reason: "the old app may still read the column".into(),
                        words: "Skip 0042 and take legacy_id out of this PR.".into(),
                    },
                ],
                pick: Some(AskPick {
                    key: "b".into(),
                    reason: "a column drop cannot be undone".into(),
                }),
            },
        }
    }

    fn agent_row(terminal: u32) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            task: None,
            news: false,
            terminal,
            name: None,
            workspace: 1,
            agent: true,
            program: Some("claude".to_string()),
            behind: None,
            state: "idle".to_string(),
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
            activity: None,
            pgid: None,
            discards: None,
        }
    }

    fn pane(n: usize) -> Pane {
        let mut p = Pane::default();
        let names = ["migrations", "web-app", "api-server"];
        p.take(
            (0..n)
                .map(|i| card(i as u32 + 1, names[i % 3], "open"))
                .collect(),
            Vec::new(),
        );
        p
    }

    fn ctx<'a>(rows: &'a [AgentRow], spaces: &'a [WorkspaceRow]) -> Ctx<'a> {
        Ctx {
            rows,
            spaces,
            now_unix: 1_000,
            now: Instant::now(),
            fade: false,
            board_above: false,
        }
    }

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

    fn text(v: &View) -> String {
        v.rows
            .iter()
            .map(|r| strip(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_pane_is_exactly_its_size_at_every_size_and_never_empty_handed() {
        for n in [0, 1, 3, 7] {
            let mut p = pane(n);
            p.focus = true;
            for cols in 48..=70 {
                for rows in 11..=60 {
                    let v = view(&p, &ctx(&[], &[]), cols, rows);
                    assert_eq!(v.rows.len(), rows, "{n} cards {cols}x{rows}");
                    for (i, r) in v.rows.iter().enumerate() {
                        assert_eq!(
                            width::width(&strip(r)),
                            cols,
                            "{n} cards {cols}x{rows} row {i}: {:?}",
                            strip(r)
                        );
                    }
                    assert!(
                        v.targets.iter().all(|t| t.row < rows - 2 && t.x1 <= cols),
                        "a target inside the box at {cols}x{rows}"
                    );
                }
            }
        }
    }

    #[test]
    fn three_cards_show_the_list_the_open_card_and_the_text_box() {
        let p = pane(3);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        for want in [
            "questions  3  board",
            "1 migrations",
            "2 web-app",
            "3 api-server",
            "Run 0042 on staging now?",
            "[a] run now",
            "[b] wait for you",
            "sends: Do not run 0042 yet",
            "qass picks b: a column drop cannot be undone",
            "type a different answer",
            "t to type",
            "waiting 6 min",
            "why now",
        ] {
            assert!(t.contains(want), "{want:?} in\n{t}");
        }
    }

    #[test]
    fn an_empty_inbox_says_so_and_a_missing_one_says_why() {
        let mut p = Pane::default();
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("reading the inbox"));
        p.take(Vec::new(), Vec::new());
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("Nothing waits for you."));
        p.unsupported = true;
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("restart qoop for the inbox"));
        assert_eq!(p.count(), None);
    }

    #[test]
    fn keys_move_the_open_card_and_the_previewed_option() {
        let now = Instant::now();
        let mut p = pane(3);
        p.focus = true;
        assert_eq!(p.open_card().unwrap().card, "E1");
        p.key(b"j", 1, now);
        p.key(b"j", 1, now);
        p.key(b"j", 1, now);
        assert_eq!(p.open_card().unwrap().card, "E3", "stops at the last");
        p.key(b"k", 1, now);
        assert_eq!(p.open_card().unwrap().card, "E2");
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("sends: Do not run"), "the pick previews first");
        p.key(b"\x1b[B", 1, now);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("sends: Skip 0042"), "{t}");
        p.key(b"\x1b[A", 1, now);
        p.key(b"\x1b[A", 1, now);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("sends: Run 0042 on staging now"), "{t}");
    }

    #[test]
    fn one_key_holds_an_answer_with_its_words_and_u_takes_it_back() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"b", 1, t0);
        assert!(p.holding());
        let v = view(
            &p,
            &Ctx {
                now: t0,
                ..ctx(&[], &[])
            },
            50,
            36,
        );
        let t = text(&v);
        assert!(
            t.contains("sending to migrations in 5 s: Do not") && t.contains("u undo"),
            "{t}"
        );
        // Nothing is sent while the hold runs.
        assert_eq!(p.tick(t0 + Duration::from_secs(2)).0, None);
        p.key(b"u", 1, t0 + Duration::from_secs(3));
        assert!(!p.holding());
        assert_eq!(
            p.tick(t0 + Duration::from_secs(9)).0,
            None,
            "taken back: never sent"
        );
        // Held again, and it goes when the time is up, with the words he saw.
        p.key(b"b", 1, t0);
        let (sent, _) = p.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite {
            action:
                AskOp::Answer {
                    card,
                    option,
                    text,
                    seen,
                },
            terminal: None,
            token: None,
            ..
        }) = sent
        else {
            panic!("an answer goes out")
        };
        assert_eq!(
            (card.as_str(), option.as_deref(), text),
            ("E1", Some("b"), None)
        );
        assert_eq!(
            seen.as_deref(),
            Some("Do not run 0042 yet; stay idle and wait for the owner.")
        );
    }

    #[test]
    fn his_own_words_go_through_the_one_box() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        p.key(b"t", 1, t0);
        assert!(p.typing());
        p.key(b"run it", 1, t0);
        p.key(b"\x7f", 1, t0);
        p.key(b"\x7f", 1, t0);
        p.key(b"twice", 1, t0);
        let v = view(&p, &ctx(&[], &[]), 50, 36);
        assert!(text(&v).contains("run twice\u{2588}"), "{}", text(&v));
        assert!(v.cursor.is_some());
        p.key(b"\r", 1, t0);
        assert!(!p.typing() && p.holding());
        let (sent, _) = p.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite {
            action: AskOp::Answer { option, text, .. },
            ..
        }) = sent
        else {
            panic!("an answer goes out")
        };
        assert_eq!((option, text.as_deref()), (None, Some("run twice")));
        // An empty box sends nothing, and esc stops typing.
        p.key(b"t", 1, t0);
        p.key(b"\r", 1, t0);
        assert!(!p.holding());
        p.key(b"\x1b", 1, t0);
        assert!(!p.typing());
    }

    #[test]
    fn the_prefix_a_mouse_report_and_the_exit_keys_are_not_typed() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        assert_eq!(p.key(b"\x01", 1, t0), Out::Pass(b"\x01".to_vec()));
        assert!(matches!(p.key(b"\x1b[<0;5;5M", 1, t0), Out::Pass(_)));
        p.key(b"t", 1, t0);
        assert_eq!(
            p.key(b"\x01", 1, t0),
            Out::Pass(b"\x01".to_vec()),
            "even while typing"
        );
        p.key(b"\x1b", 1, t0);
        assert_eq!(p.key(b"\t", 1, t0), Out::ToChat);
        assert_eq!(p.key(b"\x1b", 1, t0), Out::Closed);
        assert_eq!(p.key(b"\r", 1, t0), Out::Jump(1));
        // Letters that mean nothing are swallowed, never typed into the chat behind.
        assert_eq!(p.key(b"z", 1, t0), Out::Stay);
    }

    #[test]
    fn a_passed_card_cannot_be_answered_again_and_says_where_it_is() {
        let t0 = Instant::now();
        let mut p = Pane::default();
        let mut c = card(1, "cv", "passed");
        c.closed_at = Some(940);
        p.take(vec![c], Vec::new());
        p.focus = true;
        p.key(b"a", 1, t0);
        p.key(b"t", 1, t0);
        assert!(!p.holding() && !p.typing());
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("passed to qass, 1 min ago") && t.contains("waiting for qass"),
            "{t}"
        );
        assert_eq!(p.waiting(), 0, "a passed card is not waiting for him");
    }

    #[test]
    fn the_daemon_answers_become_notices_and_a_busy_qass_names_the_words_to_type() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        p.key(b"a", 1, t0);
        let (sent, _) = p.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite { id, .. }) = sent else {
            panic!("sent")
        };
        assert!(p.answered(&ControlResponse::AskWritten {
            id: id.clone(),
            card: "E1".into(),
            result: AskResult::Busy,
        }));
        assert!(p.stale, "the inbox is read again");
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("qass is busy, type migrations A in the chat"),
            "{t}"
        );
        // A refusal is shown as the daemon worded it.
        p.key(b"a", 1, t0);
        let (sent, _) = p.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite { id, .. }) = sent else {
            panic!("sent")
        };
        p.answered(&ControlResponse::Failed {
            id,
            reason: "E1 changed since you read it; look again".into(),
        });
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 36)).contains("changed since you read it"));
        // An older daemon: the pane says so.
        let mut old = Pane::default();
        assert!(old.answered(&ControlResponse::Failed {
            id: String::new(),
            reason: "unknown variant `AskRead`, expected one of `X`".into(),
        }));
        assert!(old.unsupported);
    }

    #[test]
    fn a_fresh_read_keeps_the_open_card_and_drops_a_hold_for_a_card_that_left() {
        let t0 = Instant::now();
        let mut p = pane(3);
        p.key(b"j", 1, t0);
        p.key(b"a", 1, t0);
        assert!(p.holding());
        let kept: Vec<AskInfo> = p.live.iter().filter(|c| c.card != "E1").cloned().collect();
        p.take(kept.clone(), Vec::new());
        assert_eq!(p.open_card().unwrap().card, "E2");
        assert!(p.holding());
        p.take(
            kept.into_iter().filter(|c| c.card != "E2").collect(),
            Vec::new(),
        );
        assert!(!p.holding(), "the card is gone");
    }

    #[test]
    fn old_cards_dim_and_the_rows_tag_what_changed() {
        let mut p = pane(1);
        p.live[0].opened_at = 1_000 - 26 * 3600;
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("waiting 26 h") && t.contains("1 migrations"),
            "{t}"
        );
        // The old card's list row is painted in the calm ink, a fresh one's in the focused ink.
        let row_of = |p: &Pane| {
            view(p, &ctx(&[], &[]), 50, 36)
                .rows
                .into_iter()
                .find(|r| strip(r).contains("1 migrations"))
                .unwrap()
        };
        let old = row_of(&p);
        p.live[0].opened_at = 1_000 - 360;
        let fresh = row_of(&p);
        let (calm, lit) = (theme::fg(Role::Detail), theme::fg(Role::Focused));
        assert!(old.contains(&calm) && !old.contains(&lit), "old: {old:?}");
        assert!(fresh.contains(&lit), "fresh: {fresh:?}");
        // Never closed: still a card waiting for him.
        assert_eq!(p.waiting(), 1);
        assert_eq!(age_short(26 * 3600), "26h");
        assert_eq!(age_short(80 * 3600), "3d");
    }

    #[test]
    fn clicks_land_on_what_the_view_drew() {
        let p = pane(3);
        let v = view(&p, &ctx(&[], &[]), 50, 36);
        let rect = Rect {
            x: 0,
            y: 7,
            cols: 50,
            rows: 36,
        };
        let row_of = |needle: &str| {
            v.rows
                .iter()
                .position(|r| strip(r).contains(needle))
                .unwrap()
        };
        assert_eq!(
            hit_at(&v, rect, 7 + row_of("2 web-app"), 10),
            Some(Hit::Card(1))
        );
        assert_eq!(
            hit_at(&v, rect, 7 + row_of("[b] wait"), 10),
            Some(Hit::Option(1))
        );
        assert_eq!(
            hit_at(&v, rect, 7 + row_of("t to type"), 10),
            Some(Hit::Box)
        );
        let tabs = row_of("questions");
        assert_eq!(hit_at(&v, rect, 7 + tabs, 2 + 16), Some(Hit::Board));
        assert_eq!(hit_at(&v, rect, 7 + tabs, 3), None);
        assert_eq!(hit_at(&v, rect, 6, 10), None, "above the box");
        let mut p = pane(3);
        let now = Instant::now();
        p.click(Hit::Card(2), now);
        assert_eq!(p.open_card().unwrap().card, "E3");
        p.click(Hit::Option(0), now);
        assert!(p.holding());
        let mut q = pane(1);
        q.click(Hit::Box, now);
        assert!(q.typing());
    }

    #[test]
    fn a_short_pane_gives_up_the_handled_list_before_the_card() {
        let mut p = pane(3);
        p.ended = vec![card(9, "docs", "closed")];
        let tall = text(&view(&p, &ctx(&[], &[]), 50, 50));
        assert!(tall.contains("handled for you"), "{tall}");
        let short = text(&view(&p, &ctx(&[], &[]), 50, 22));
        assert!(!short.contains("handled for you"), "{short}");
        assert!(
            short.contains("[a] run now") && short.contains("type a different answer"),
            "{short}"
        );
    }

    #[test]
    fn a_notice_stands_a_few_seconds_and_the_key_line_comes_back() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        p.key(b"a", 1, t0);
        p.key(b"u", 1, t0);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("taken back") && !t.contains("j k cards"), "{t}");
        assert!(p.wake_at().is_some(), "a clock to let it go");
        assert!(!p.tick(t0 + Duration::from_secs(1)).1);
        assert!(
            p.tick(t0 + NOTICE + Duration::from_secs(1)).1,
            "repainted once to let go"
        );
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("j k cards") && !t.contains("taken back"), "{t}");
        assert_eq!(p.wake_at(), None, "nothing armed again");
    }

    #[test]
    fn handled_cards_say_how_they_ended_in_words() {
        let mut p = pane(0);
        let mut chat = card(1, "korea", "closed");
        chat.note = Some("chat".into());
        chat.closed_at = Some(1_000 - 3_600 * 3);
        let mut mine = card(2, "cv", "answered");
        mine.option = Some("b".into());
        mine.closed_at = Some(1_000 - 120);
        p.take(Vec::new(), vec![mine, chat]);
        let t = text(&view(&p, &ctx(&[], &[]), 56, 30));
        for want in [
            "you chose b",
            "2m",
            "answered in chat",
            "3h",
            "handled for you 2",
        ] {
            assert!(t.contains(want), "{want:?} in\n{t}");
        }
    }

    #[test]
    fn busy_names_what_was_sent_not_what_is_open_now() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"c", 1, t0);
        let (sent, _) = p.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite { id, .. }) = sent else {
            panic!("sent")
        };
        // He moved on while it was out.
        p.key(b"j", 1, t0);
        p.answered(&ControlResponse::AskWritten {
            id,
            card: "E1".into(),
            result: AskResult::Busy,
        });
        let t = text(&view(&p, &ctx(&[], &[]), 56, 36));
        assert!(
            t.contains("qass is busy, type migrations C in the chat"),
            "{t}"
        );
        // His own words are typed after the handle, as he would type them.
        let mut q = pane(1);
        q.focus = true;
        q.key(b"t", 1, t0);
        q.key(b"run it twice", 1, t0);
        q.key(b"\r", 1, t0);
        let (sent, _) = q.tick(t0 + HOLD);
        let Some(ControlRequest::AskWrite { id, .. }) = sent else {
            panic!("sent")
        };
        q.answered(&ControlResponse::AskWritten {
            id,
            card: "E1".into(),
            result: AskResult::Busy,
        });
        assert!(text(&view(&q, &ctx(&[], &[]), 70, 36)).contains("type migrations run it twice"));
    }

    #[test]
    fn a_hold_marks_its_own_card_and_names_who_it_goes_to_and_leaving_takes_it_back() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"b", 1, t0);
        let marked = |p: &Pane| {
            let v = view(
                p,
                &Ctx {
                    now: t0,
                    ..ctx(&[], &[])
                },
                56,
                36,
            );
            v.rows
                .iter()
                .any(|r| strip(r).contains("[b] wait") && r.contains("\x1b[1m"))
        };
        assert!(marked(&p), "the held option is bold on its card");
        p.key(b"j", 1, t0);
        assert!(!marked(&p), "and not on another card's");
        assert!(
            text(&view(
                &p,
                &Ctx {
                    now: t0,
                    ..ctx(&[], &[])
                },
                56,
                36
            ))
            .contains("sending to migrations in 5 s")
        );
        p.release();
        assert!(!p.holding() && !p.focus);
        assert_eq!(p.tick(t0 + HOLD).0, None, "taken back: never sent");
    }

    #[test]
    fn words_typed_for_a_card_are_not_kept_for_the_one_that_replaced_it() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"t", 1, t0);
        p.key(b"for the first", 1, t0);
        // The card he types for is gone: the box is empty and closed for the next one.
        p.take(p.live[1..].to_vec(), Vec::new());
        assert!(!p.typing() && p.typed().is_empty());
        // Typed text changes with a move to another card, and escapes are not letters.
        p.key(b"t", 1, t0);
        p.key(b"\x1b[D", 1, t0);
        p.key(b"\x1b[200~", 1, t0);
        assert_eq!(p.typed(), "");
        p.key(b"ok", 1, t0);
        assert_eq!(p.typed(), "ok");
    }

    #[test]
    fn a_card_whose_terminal_left_is_read_again_once_and_watched_again_when_it_returns() {
        let mut p = pane(1);
        let row = agent_row;
        // No index yet: nothing to compare with.
        p.note_gone(&[], &[]);
        assert!(!p.stale);
        p.note_gone(&[row(9)], &[]);
        assert!(p.stale, "terminal 1 is not in the index");
        p.stale = false;
        p.note_gone(&[row(9)], &[]);
        assert!(!p.stale, "only once");
        p.note_gone(&[row(1)], &[]);
        p.note_gone(&[row(9)], &[]);
        assert!(p.stale, "it came back and left again");
    }
}
