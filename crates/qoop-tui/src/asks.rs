//! The question inbox pane: the cards qass keeps for the owner, each folded to its question and
//! its numbered answers, one of them opened in place.
//!
//! Beside qass's terminal (or in its place, in a narrow pane) the owner answers a card with one
//! key or click (`1` to `9`), opens it (enter) to read what he asked, what the agent did, why it
//! needs him, the whole question and what each answer sends, or replies in his own words (`r`)
//! in the one text box. Everything
//! here is a pure function of the cards the daemon sent and of what this client remembers (which
//! card is open, what is typed, the answer being held), so a test reads the pane instead of
//! looking at one.
//!
//! An answer goes at once, and a calm line says what went ("sent to korea: ..."). The daemon
//! keeps it and types it as soon as its target takes input, so nothing here waits or refuses.

use std::time::{Duration, Instant};

use qoop_runtime::control::{AgentRow, WorkspaceRow};
use qoop_runtime::control::{AskInfo, AskOp, AskResult, ControlRequest, ControlResponse};

use crate::layout::Rect;
use crate::qass_screen::{Seg, bold, edge, ink, line, seg};
use crate::sidebar;
use crate::theme::{self, Role};
use crate::width;

/// How long a word about what just happened stands in place of the key line.
const NOTICE: Duration = Duration::from_secs(6);

/// A card this old is shown dim; it is never closed for being old.
pub const OLD_SECS: i64 = 24 * 3600;

/// The id of the request that reads the inbox, and the prefix of the ones that write it.
pub const READ_ID: &str = "asks-read";
pub const WRITE_ID: &str = "asks-write";

/// The most characters of his own words one answer carries (the daemon's own limit).
const TEXT_MAX: usize = 400;
/// What he is told under his words, after the agent's name, when the card they were begun for
/// was answered, dismissed or closed elsewhere: nothing is sent.
const HANDLED_ELSEWHERE: &str = "handled elsewhere";

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
    /// The keyboard goes to qass's chat with this line to talk the card through.
    ToQass(String),
    /// Talk this card through: with the agent it is about (`talk_target`), else in qass's chat.
    Talk(Box<AskInfo>),
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
    /// The selected card is opened in place (enter); the others stay folded.
    expanded: bool,
    typing: bool,
    text: String,
    /// The card the words in the box were begun for, and its agent's handle.
    typing_for: Option<(String, String)>,
    /// Words typed for cards not open now, by card: they come back when that card is opened
    /// again and go when it is answered, put away or ends.
    drafts: std::collections::BTreeMap<String, String>,
    /// Writes made and not yet sent: the loop sends them on its next tick.
    outbox: Vec<ControlRequest>,
    /// Writes made so far: each takes its own request id, so two answers to one card in
    /// flight are told apart.
    writes: u64,
    /// What the daemon last said about an answer: shown in place of the key line.
    notice: Option<String>,
    /// When the notice is let go.
    notice_until: Option<Instant>,
    /// Answers sent and not yet answered, by request id: the card's handle and his words.
    sent: std::collections::BTreeMap<String, (String, String)>,
    /// Words on their way, by request id: the card, the words (a typed answer, or the draft an
    /// option or a dismissal took with it) and whether they were the answer sent; put back in
    /// the box if the write fails, let go once the daemon took them.
    pending: std::collections::BTreeMap<String, (String, String, bool)>,
    /// Words that can no longer be sent, their card ended, with that card's handle: the one
    /// place they go (a draft, words put back, a box left on an ended card). Added to, never
    /// replaced; drawn until an esc outside typing and the whole view lets them go.
    lost: Vec<(String, String)>,
    /// Cards already read again because their terminal left the rows.
    gone: Vec<String>,
    /// The owner brought the questions up while cards waited: they stay until the last card
    /// is answered and its word seen, or he puts them away. A card never sets it.
    pub raised: bool,
    /// The open card's question read whole (`m`), in place of its answers.
    whole: bool,
    /// The answers the last frame drew, so `a` to `d` never send one the person cannot see;
    /// `None` before a frame.
    shown: std::cell::Cell<Option<usize>>,
    /// The first row of the question read whole, scrolled by `j k`, and the last first row the
    /// last frame could show.
    whole_at: usize,
    whole_end: std::cell::Cell<usize>,
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

    /// Whether the selected card shows in full: opened with enter, or holding his words.
    pub fn is_open(&self) -> bool {
        self.expanded || self.typing || !self.text.is_empty()
    }

    /// The selected card, the one a key answers.
    pub fn open_card(&self) -> Option<&AskInfo> {
        self.live
            .get(self.selected.min(self.live.len().saturating_sub(1)))
    }

    /// Take a fresh read: the open card stays the open card when it is still there.
    pub fn take(&mut self, live: Vec<AskInfo>, ended: Vec<AskInfo>) {
        let keep = self.open_card().map(|c| c.card.clone());
        let handles: std::collections::BTreeMap<String, String> = self
            .live
            .iter()
            .map(|c| (c.card.clone(), c.body.handle.clone()))
            .collect();
        self.live = live;
        self.ended = ended;
        self.loaded = true;
        self.unsupported = false;
        self.selected = keep
            .as_ref()
            .and_then(|k| self.live.iter().position(|c| c.card == *k))
            .unwrap_or(self.selected)
            .min(self.live.len().saturating_sub(1));
        // A card that took the open one's place is read folded first.
        if keep
            .as_ref()
            .is_none_or(|k| self.open_card().is_none_or(|c| c.card != *k))
        {
            self.expanded = false;
        }
        // Words kept for a card are not for the one that took its place: they wait for their
        // card. While he types they stay, his card answered, dismissed or closed elsewhere (the
        // phone) or not: the keys stay in the box, so no letter of his becomes a command (`j` to
        // another card, `x` to put it away), and enter sends nothing ([`Pane::orphaned`]).
        if !self.typing {
            self.swap_draft(keep);
        }
        // Words kept for a card that ended (a retry put back, a draft) are never dropped
        // unseen: they go to the lost words.
        let live = &self.live;
        let (kept, ended): (
            std::collections::BTreeMap<_, _>,
            std::collections::BTreeMap<_, _>,
        ) = std::mem::take(&mut self.drafts)
            .into_iter()
            .partition(|(card, _)| live.iter().any(|c| c.card == *card));
        self.drafts = kept;
        for (card, words) in ended {
            let who = handles.get(&card).cloned().unwrap_or_default();
            self.lost.push((who, words));
        }
    }

    /// The lost words as they read: whose card ended, and the words.
    fn lost_said(&self) -> impl Iterator<Item = String> + '_ {
        self.lost.iter().map(|(who, words)| match who.is_empty() {
            true => format!("not sent, a card ended: {words}"),
            false => format!("not sent, {who}'s card ended: {words}"),
        })
    }

    /// The open card is no longer `from`: the words in the box are kept for `from`, and the
    /// open card's own words come back.
    fn swap_draft(&mut self, from: Option<String>) {
        let to = self.open_card().map(|c| c.card.clone());
        if to == from {
            return;
        }
        self.whole = false;
        let words = std::mem::take(&mut self.text);
        if let Some(from) = from.filter(|_| !words.is_empty()) {
            self.drafts.insert(from, words);
        }
        self.text = to.and_then(|c| self.drafts.remove(&c)).unwrap_or_default();
    }

    /// The card his words were begun for is no longer the open one waiting for him: it was
    /// answered, dismissed or closed elsewhere, and enter sends nothing.
    fn orphaned(&self) -> bool {
        !self.open_card().is_some_and(|c| {
            c.status == "open" && self.typing_for.as_ref().map(|(card, _)| card) == Some(&c.card)
        })
    }

    /// Typing ends. Words no card can take any more leave the box for the lost words, so a
    /// later `t` never sends them to another card.
    fn stop_typing(&mut self) {
        if self.typing && self.orphaned() && !self.text.is_empty() {
            let who = self.typing_for.as_ref().map(|(_, h)| h.clone());
            self.lost
                .push((who.unwrap_or_default(), std::mem::take(&mut self.text)));
        }
        self.typing = false;
    }

    /// A card whose terminal has left the agent rows (and sleeps in no tab) is read again, once:
    /// the daemon closed or untied it with the terminal and sends no event for that.
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
                self.pending.remove(id);
                let (handle, words) = self.sent.remove(id).unwrap_or_default();
                let said = match result {
                    AskResult::Answered => format!("sent to {handle}: {words}"),
                    AskResult::Passed => format!("sent to qass: {words}"),
                    // Only a daemon from before answers were queued says it.
                    AskResult::Busy => format!("qass is busy, answer {handle} in the chat"),
                    AskResult::Closed => "put away".to_string(),
                    _ => "done".to_string(),
                };
                self.say(said);
                self.stale = true;
                true
            }
            ControlResponse::Failed { id, reason } if self.is_mine(id) => {
                let (handle, _) = self.sent.remove(id).unwrap_or_default();
                let said = match self.pending.remove(id) {
                    // Its card ended elsewhere: no box to put them back in.
                    Some((card, words, _)) if !self.live.iter().any(|c| c.card == card) => {
                        self.lost.push((handle, words));
                        format!("not sent: {reason}")
                    }
                    // An option or a dismissal failed: the draft it took comes back.
                    Some((card, words, false)) => {
                        self.restore(card, words);
                        format!("not sent: {reason}")
                    }
                    Some((card, words, true)) => {
                        let here = self.open_card().is_some_and(|c| c.card == card);
                        self.restore(card, words);
                        if here {
                            format!("not sent, t and enter retry: {reason}")
                        } else {
                            format!("not sent, kept on {handle}'s card: {reason}")
                        }
                    }
                    None => format!("not sent: {reason}"),
                };
                // A failure stays until esc or typing: read whole, not missed.
                self.notice = Some(said);
                self.notice_until = None;
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
        self.sent.contains_key(id)
    }

    /// Words a failed write took out of the box go back to their card, ahead of any typed since:
    /// into the box when that card is open, else kept for the card.
    fn restore(&mut self, card: String, words: String) {
        let open = self.open_card().is_some_and(|c| c.card == card);
        let kept = if open {
            &mut self.text
        } else {
            self.drafts.entry(card).or_default()
        };
        *kept = match kept.is_empty() {
            true => words,
            false => format!("{words} {kept}"),
        };
    }

    /// The moment the inbox next has something to do by itself: a write to send now or a
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

    /// The keyboard leaves the inbox.
    pub fn release(&mut self) {
        self.focus = false;
        self.stop_typing();
    }

    /// Whether an answer is on its way, just answered or being typed: the questions stay up
    /// through all three, so the last answer's word is still seen and no letter of his goes to
    /// The words not sent when the daemon goes, one line each with whose card: answers on
    /// their way, then the lost words.
    pub fn unsent(&self) -> Vec<String> {
        self.sent
            .values()
            .filter(|(_, words)| !words.is_empty())
            .map(|(handle, words)| format!("not sent to {handle}: {words}"))
            .chain(self.lost_said())
            .collect()
    }

    /// qass's chat when his card is handled elsewhere.
    pub fn busy(&self) -> bool {
        !self.outbox.is_empty()
            || !self.sent.is_empty()
            || self.notice.is_some()
            || self.typing
            || !self.lost.is_empty()
    }

    /// Write now: the one request every answer and every dismissal goes out by.
    fn write(&mut self, action: AskOp, handle: String, words: String) {
        let card = match &action {
            AskOp::Answer { card, .. } | AskOp::Dismiss { card } => card.clone(),
            _ => return,
        };
        self.writes += 1;
        let id = format!("{WRITE_ID}-{card}-{}", self.writes);
        // The words that go with this write: its own text, else the card's draft.
        let open = self.open_card().is_some_and(|c| c.card == card);
        let (kept, typed) = match &action {
            AskOp::Answer {
                text: Some(text), ..
            } => (text.clone(), true),
            _ if open => (self.text.clone(), false),
            _ => (self.drafts.get(&card).cloned().unwrap_or_default(), false),
        };
        if !kept.is_empty() {
            self.pending.insert(id.clone(), (card.clone(), kept, typed));
        }
        self.sent.insert(id.clone(), (handle, words));
        self.outbox.push(ControlRequest::AskWrite {
            id,
            terminal: None,
            token: None,
            action,
        });
        self.notice = None;
        self.notice_until = None;
        self.typing = false;
        self.text.clear();
        self.drafts.remove(&card);
    }

    fn send_answer(&mut self, option: Option<usize>, text: Option<String>) -> bool {
        let Some(card) = self.open_card() else {
            return false;
        };
        if card.status != "open" {
            return false;
        }
        let (words, option, text, seen) =
            match (option.and_then(|i| card.body.options.get(i)), text) {
                (_, Some(text)) => (text.clone(), None, Some(text), None),
                (Some(o), None) => (
                    o.words.clone(),
                    Some(o.key.clone()),
                    None,
                    Some(o.words.clone()),
                ),
                (None, None) => return false,
            };
        let (card, handle) = (card.card.clone(), card.body.handle.clone());
        self.write(
            AskOp::Answer {
                card,
                option,
                text,
                seen,
            },
            handle,
            words,
        );
        true
    }

    fn dismiss(&mut self) -> bool {
        let Some(card) = self.open_card() else {
            return false;
        };
        let (card, handle) = (card.card.clone(), card.body.handle.clone());
        self.write(AskOp::Dismiss { card }, handle, String::new());
        true
    }

    /// Answer with option `at` of the open card.
    pub fn press_option(&mut self, at: usize) -> bool {
        self.send_answer(Some(at), None)
    }

    /// Open card number `at` of the list.
    pub fn select(&mut self, at: usize) {
        if at < self.live.len() {
            // The words in the box are for the card typed into, else for the open one.
            let from = match (&self.typing_for, self.typing) {
                (Some((card, _)), true) => Some(card.clone()),
                _ => self.open_card().map(|c| c.card.clone()),
            };
            self.selected = at;
            self.expanded = false;
            self.typing = false;
            self.swap_draft(from);
            // A failure stays over a move between cards: only esc or typing takes it.
            if self.notice_until.is_some() {
                self.notice = None;
                self.notice_until = None;
            }
        }
    }

    /// Start typing in the one text box.
    pub fn start_typing(&mut self) {
        if let Some(c) = self.open_card().filter(|c| c.status == "open") {
            self.typing_for = Some((c.card.clone(), c.body.handle.clone()));
            self.typing = true;
            // The box and its caret are on screen while typing.
            self.expanded = true;
            self.whole = false;
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

    /// A key, or the bytes of several. `prefix` is the interface's prefix byte, which with a
    /// mouse report or a detach is not the inbox's and goes back.
    pub fn key(&mut self, bytes: &[u8], prefix: u8) -> Out {
        if bytes.first() == Some(&prefix) || bytes.starts_with(b"\x1b[<") || bytes == b"\x11" {
            return Out::Pass(bytes.to_vec());
        }
        // A failure said stays until esc (or typing, which takes the line); the lost words go
        // on an esc of their own. Not in the whole view, where esc goes back to the card, nor
        // while typing, where it stops the typing first.
        if bytes == b"\x1b" && !self.whole {
            if self.notice_until.is_none() {
                self.notice = None;
            }
            if !self.typing && !self.lost.is_empty() {
                self.lost.clear();
                return Out::Stay;
            }
        }
        if self.typing {
            return self.type_key(bytes);
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
            match self.act(key) {
                Out::Stay => {}
                other => {
                    out = other;
                    break;
                }
            }
        }
        out
    }

    fn act(&mut self, key: Key) -> Out {
        match key {
            // Esc in the card read whole goes back to the card, as `m` does.
            Key::Esc if self.whole => {
                self.whole = false;
                Out::Stay
            }
            Key::Esc => Out::Closed,
            Key::Char(b'j' | b'k') | Key::Down | Key::Up if self.whole => {
                let down = matches!(key, Key::Char(b'j') | Key::Down);
                self.whole_at = if down {
                    (self.whole_at + 1).min(self.whole_end.get())
                } else {
                    self.whole_at.saturating_sub(1)
                };
                Out::Stay
            }
            Key::Char(b'\t') => Out::ToChat,
            Key::Char(b'j') | Key::Down => {
                self.step(true);
                Out::Stay
            }
            Key::Char(b'k') | Key::Up => {
                self.step(false);
                Out::Stay
            }
            // `1` to `9`, or `a` to `d` as before: the answer drawn with that number. One not
            // drawn (cut to the room) takes no key.
            Key::Char(c @ (b'1'..=b'9' | b'a'..=b'd')) => {
                let at = (if c.is_ascii_digit() {
                    c - b'1'
                } else {
                    c - b'a'
                }) as usize;
                if self.shown.get().is_none_or(|n| at < n) {
                    self.press_option(at);
                }
                Out::Stay
            }
            Key::Char(b'm') => {
                self.whole = !self.whole;
                self.whole_at = 0;
                Out::Stay
            }
            // `t` types as it always did; `r` (reply) is the same.
            Key::Char(b't' | b'r') => {
                self.start_typing();
                Out::Stay
            }
            Key::Char(b'x') => {
                self.dismiss();
                Out::Stay
            }
            // Enter opens the card in place, and folds it again.
            Key::Char(b'\r' | b'\n') if self.whole => Out::Stay,
            Key::Char(b'\r' | b'\n') => {
                if self.open_card().is_some() {
                    self.expanded = !self.expanded;
                }
                Out::Stay
            }
            // `g` goes to the card's tab, which enter did before it opened the card.
            Key::Char(b'g') => match self.open_card().and_then(|c| c.terminal) {
                Some(terminal) => Out::Jump(terminal),
                None => Out::Stay,
            },
            Key::Char(b'o') => match self.open_card() {
                Some(card) => Out::Talk(Box::new(card.clone())),
                None => Out::Stay,
            },
            Key::Char(_) => Out::Stay,
        }
    }

    fn type_key(&mut self, bytes: &[u8]) -> Out {
        if bytes == b"\x1b" {
            self.stop_typing();
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
                    if !text.is_empty() && !self.orphaned() {
                        self.send_answer(None, Some(text));
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
    pub fn click(&mut self, hit: Hit) -> Out {
        match hit {
            Hit::Card(at) if at == self.selected && self.open_card().is_some() => {
                self.expanded = !self.expanded;
            }
            Hit::Card(at) => {
                self.select(at);
                self.expanded = true;
            }
            Hit::Option(at) => {
                self.press_option(at);
            }
            Hit::Answer(card, at) => {
                if card != self.selected {
                    self.select(card);
                }
                self.press_option(at);
            }
            Hit::Reply(card) => {
                if card != self.selected {
                    self.select(card);
                }
                self.start_typing();
            }
            Hit::Box => self.start_typing(),
            Hit::Board => {
                self.say("the board is not here yet");
            }
            // The client folds the column: nothing of the pane's own changes.
            Hit::Fold => {}
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
    /// A card's heading, by position: opens it in place, or folds it when it is the open one.
    Card(usize),
    /// An option of the open card, by position.
    Option(usize),
    /// An answer on a folded card's row: the card by position, then the option.
    Answer(usize, usize),
    /// `[r] Reply` on a folded card's row, by position.
    Reply(usize),
    /// The text box.
    Box,
    /// The second tab.
    Board,
    /// The title on qass's split tab: folds the questions back to their strip.
    Fold,
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

pub(crate) fn age_short(secs: i64) -> String {
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

/// An item's plan in the words every screen says it in, and its ink: `plan needs you` once it
/// waited too long for qass, `plan in review` while qass reads it, `plan ok · qass` or `plan ok
/// · you` approved. None without a plan file.
pub fn plan_word(plan: &str, by: Option<&str>) -> Option<(String, Role)> {
    let word = match (plan, by) {
        ("stale" | "asked", _) => "plan needs you",
        ("written" | "changed", _) => "plan in review",
        ("approved", Some("owner")) => "plan ok \u{b7} you",
        ("approved", Some("qass")) => "plan ok \u{b7} qass",
        ("approved", _) => "plan ok",
        _ => return None,
    };
    let role = match plan {
        "stale" | "asked" => Role::Blocked,
        "approved" => Role::Done,
        _ => Role::FaintText,
    };
    Some((word.to_string(), role))
}

/// A plan card: qass asks him to approve its item's plan, which is not approved yet.
fn plan_card(card: &AskInfo) -> bool {
    card.body.handle == "plan"
        && card
            .item
            .as_ref()
            .is_some_and(|i| matches!(i.plan.as_str(), "written" | "stale" | "changed" | "asked"))
}

/// The line under a card's heading for the item it is about: `KEY · Build 2/3 · plan needs
/// you`. A plan card's plan waits on him whatever qass's own state for it says.
fn item_line(card: &AskInfo) -> Option<Vec<Seg>> {
    let item = card.item.as_ref()?;
    let dot = || seg(Role::FaintText, " \u{b7} ");
    let mut segs = vec![seg(Role::Detail, item.key.clone())];
    if let Some(p) = &item.phase {
        segs.push(dot());
        segs.push(seg(Role::Detail, format!("{} {}/{}", p.name, p.n, p.of)));
    }
    let plan = if plan_card(card) {
        Some(("plan needs you".to_string(), Role::Blocked))
    } else {
        plan_word(&item.plan, None)
    };
    if let Some((word, role)) = plan {
        segs.push(dot());
        segs.push(seg(role, word));
    }
    Some(segs)
}

/// A tag on the open card that says why it may not be what it was, from the rows alone.
fn tag_of(card: &AskInfo, ctx: &Ctx) -> Option<String> {
    if card.status == "queued" {
        return Some("sent when it is free".to_string());
    }
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

/// The most rows each card text other than the question takes (a reason, what was asked): a text
/// that needs more ends in `\u{2026}`. The question takes all it needs while the answers, the
/// box and the key line still fit; else it is cut, and `m` reads it whole.
const SIDE_ROWS: usize = 8;

/// The rows each card text needs at its width, as many as `SIDE_ROWS` allows; none for a part
/// the card has not got (no `asked`, a generic `why`).
struct Lens {
    ask: usize,
    asked: usize,
    doing: usize,
    why: usize,
    /// Per option: the rows its reason, its `suggested because` and the words it sends take.
    reasons: Vec<usize>,
    becauses: Vec<usize>,
    sends: Vec<usize>,
    /// The card has a row under its title (a tag such as `asleep`, or its item): it is kept.
    tag: bool,
    /// A label takes a row of its own over its text: a narrow pane.
    stacked: bool,
}

/// How the pane is divided between the parts, given its height.
struct Budget {
    /// Rows of the question: all it needs.
    ask: usize,
    /// The most rows each side text takes: it falls to 1, a cut row, as the pane gets short.
    side: usize,
    /// How many other cards stay folded around the open one.
    others: usize,
    asked: bool,
    why: bool,
    gap_top: bool,
    gap_box: bool,
    because: bool,
    sends: bool,
    doing: bool,
    /// The text box, which a short pane drops while it holds no words (`r` brings it back).
    text_box: bool,
    /// The blank row under the card's title, where a tag would stand.
    head_gap: bool,
    handled: usize,
}

/// `others` is how many other cards could stay folded, each `folded` rows tall.
fn budget(inner_rows: usize, others: usize, folded: usize, words: bool, lens: &Lens) -> Budget {
    let mut b = Budget {
        ask: lens.ask,
        side: SIDE_ROWS,
        others: others.min(4),
        asked: lens.asked > 0,
        why: lens.why > 0,
        gap_top: true,
        gap_box: true,
        because: lens.becauses.iter().any(|&n| n > 0),
        sends: true,
        doing: lens.doing > 0,
        text_box: true,
        head_gap: true,
        handled: 5,
    };
    let need = |b: &Budget| {
        let label = usize::from(lens.stacked);
        let part = |on: bool, n: usize| if on { label + n.min(b.side) } else { 0 };
        let rows = |on: bool, v: &[usize]| {
            if on {
                v.iter().map(|&n| n.min(b.side)).sum::<usize>()
            } else {
                0
            }
        };
        // The title, the row under it, the thread, the question, every answer with its parts,
        // and the box.
        let detail = 1
            + usize::from(b.head_gap)
            + part(b.asked, lens.asked)
            + part(b.doing, lens.doing)
            + part(b.why, lens.why)
            + usize::from(b.gap_top)
            + b.ask
            + rows(true, &lens.reasons)
            + rows(b.because, &lens.becauses)
            + rows(b.sends, &lens.sends)
            + usize::from(b.gap_box)
            + 3 * usize::from(b.text_box);
        let handled = if b.handled > 0 { b.handled + 2 } else { 0 };
        // tabs, rule, the other cards, the open one, what was handled, the key line.
        2 + b.others * folded + detail + handled + 1
    };
    while need(&b) > inner_rows {
        if b.handled > 0 {
            b.handled = b.handled.saturating_sub(1);
            if b.handled < 2 {
                b.handled = 0;
            }
        } else if b.side > 1 {
            b.side -= 1;
        } else if b.others > 2 {
            b.others -= 1;
        } else if b.gap_top {
            b.gap_top = false;
        } else if b.asked {
            b.asked = false;
        } else if b.gap_box {
            b.gap_box = false;
        } else if b.why {
            b.why = false;
        } else if b.because {
            b.because = false;
        } else if b.others > 0 {
            // The open card is the one in its head: the others go before the question is cut.
            b.others -= 1;
        } else if b.sends {
            b.sends = false;
        } else if b.doing {
            b.doing = false;
        } else if b.text_box && !words {
            b.text_box = false;
        } else if b.head_gap && !lens.tag {
            b.head_gap = false;
        } else {
            break;
        }
    }
    // The answers, the box and the key line keep their rows: the question takes what is left,
    // cut with `m` to read it whole.
    let over = need(&b).saturating_sub(inner_rows);
    b.ask = b.ask.saturating_sub(over).max(1);
    b
}

/// A folded card's question shares its row with who asks from this width; narrower it has a row
/// of its own.
const INLINE_W: usize = 56;
/// The `space:tab` of a folded card shows from this width.
const WHERE_W: usize = 72;
/// A thread's labels share a row with their text from this width; narrower they stack.
const LABEL_W: usize = 56;
/// The width of a thread's label column when they share a row.
const LABEL_COL: usize = 18;

/// The most rows a folded card's question wraps to under its head, in a narrow pane.
const QUESTION_ROWS: usize = 3;

/// Rows a folded card takes at most: its head with the question beside it when there is room,
/// else under it on up to [`QUESTION_ROWS`] rows, then its numbered answers.
fn folded_h(cw: usize) -> usize {
    if cw >= INLINE_W { 2 } else { 2 + QUESTION_ROWS }
}

/// A card's question as one row of words.
fn question_line(card: &AskInfo) -> String {
    card.body
        .asks
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One folded card: its head row (the marker, who asks, where, now or later and how long, the
/// question), then its answers on a row of their own, `[1] Enable both (suggested)  [2] ...
/// [r] Reply`, each a click that sends it. Returns how many answers were drawn whole in
/// order, which the number keys reach.
#[allow(clippy::too_many_arguments)]
fn folded(
    card: &AskInfo,
    at: usize,
    on: bool,
    ctx: &Ctx,
    cw: usize,
    alone: Option<usize>,
    wrap: &impl Fn(&[Seg]) -> String,
    out: &mut Vec<String>,
    targets: &mut Vec<Target>,
) -> usize {
    let calm = ctx.now_unix - card.opened_at >= OLD_SECS || card.status != "open";
    let ask_ink = if calm { Role::FaintText } else { Role::Text };
    let name_w = 12usize.min(cw / 4);
    let name = width::fit(
        &cut_dots(&sidebar::chip_name(&name_of(card, ctx)), name_w),
        name_w,
    );
    let name_ink = match (on, calm) {
        (true, true) => Role::Detail,
        (true, false) => Role::Focused,
        (false, _) => ask_ink,
    };
    let age = age_short(ctx.now_unix - card.opened_at);
    let when = format!("{:<5} {:>4}", card.body.urgency, age);
    let when_ink = if card.body.urgency == "now" && !calm {
        Role::Blocked
    } else {
        Role::FaintText
    };
    let inline = cw >= INLINE_W;
    let mut head = vec![
        if on {
            bold(Role::Brand, "\u{203a} ")
        } else {
            seg(Role::Text, "  ")
        },
        Seg {
            bold: on,
            ..seg(name_ink, name)
        },
        seg(Role::Text, "  "),
    ];
    let mut used = 2 + name_w + 2;
    if inline && cw >= WHERE_W {
        let tab = card.terminal.map(|t| t.to_string()).unwrap_or_default();
        let place = match space_of(card, ctx) {
            Some(space) => format!("{space}:{tab}"),
            None => format!("T{tab}"),
        };
        head.push(seg(Role::FaintText, width::fit(&cut_dots(&place, 14), 14)));
        head.push(seg(Role::Text, "  "));
        used += 16;
    }
    let asks = question_line(card);
    if inline {
        head.push(seg(when_ink, when.clone()));
        head.push(seg(Role::Text, "  "));
        used += width::width(&when) + 2;
        head.push(seg(ask_ink, cut_dots(&asks, cw.saturating_sub(used))));
    } else {
        let gap = cw.saturating_sub(used + width::width(&when));
        head.push(seg(Role::Text, " ".repeat(gap)));
        head.push(seg(when_ink, when));
    }
    let mut rows = vec![head];
    if !inline {
        // Alone in the pane, the whole question wraps into the rows there are.
        let q_rows = alone.map_or(QUESTION_ROWS, |rows| {
            rows.saturating_sub(2).max(QUESTION_ROWS)
        });
        for line in wrap_dots(&asks, cw.saturating_sub(2), q_rows) {
            rows.push(vec![seg(Role::Text, "  "), seg(ask_ink, line)]);
        }
    }
    for segs in &rows {
        targets.push(Target {
            row: out.len(),
            x0: 2,
            x1: 2 + cw,
            hit: Hit::Card(at),
        });
        out.push(wrap(segs));
    }
    // The answers, as many whole as the row holds.
    const INDENT: usize = 4;
    let live = card.status == "open";
    // A card that is not his to answer (passed to qass, queued) says so where the answers would
    // stand.
    if !live {
        let tag = tag_of(card, ctx).unwrap_or_default();
        out.push(wrap(&[
            seg(Role::Text, " ".repeat(INDENT)),
            seg(Role::Blocked, cut_dots(&tag, cw.saturating_sub(INDENT))),
        ]));
        return 0;
    }
    let mut segs = vec![seg(Role::Text, " ".repeat(INDENT))];
    let mut x = INDENT;
    let mut shown = 0;
    let mut cut = false;
    for (i, o) in card.body.options.iter().enumerate() {
        let suggested = card.body.pick.as_ref().is_some_and(|p| p.key == o.key);
        let num = format!("[{}] ", i + 1);
        let mark = if suggested { " (suggested)" } else { "" };
        let mut sep = if x > INDENT { 2 } else { 0 };
        let full = width::width(&num) + width::width(&o.label) + width::width(mark);
        if x + sep + full > cw && x > INDENT {
            // Alone in the pane, an answer that does not fit goes on the next row: every
            // answer stays in sight. Among other cards the row ends in dots.
            if alone.is_none() {
                cut = true;
                break;
            }
            out.push(wrap(&segs));
            segs = vec![seg(Role::Text, " ".repeat(INDENT))];
            (x, sep) = (INDENT, 0);
        }
        let room = cw.saturating_sub(x + sep);
        let (label, mark) = if full <= room {
            (o.label.clone(), mark)
        } else {
            (
                cut_dots(&o.label, room.saturating_sub(width::width(&num))),
                "",
            )
        };
        let end = x + sep + width::width(&num) + width::width(&label) + width::width(mark);
        if sep > 0 {
            segs.push(seg(Role::Text, "  "));
        }
        segs.push(seg(if live { Role::Brand } else { Role::FaintText }, num));
        segs.push(seg(if live { ask_ink } else { Role::FaintText }, label));
        segs.push(seg(if live { Role::Done } else { Role::FaintText }, mark));
        targets.push(Target {
            row: out.len(),
            x0: 2 + x + sep,
            x1: 2 + end,
            hit: Hit::Answer(at, i),
        });
        (x, shown) = (end, shown + 1);
        if x >= cw {
            break;
        }
    }
    if cut && x + 1 < cw {
        segs.push(seg(Role::FaintText, "\u{2026}"));
        x += 1;
    }
    if live && !cut {
        let reply = "[r] Reply";
        let mut sep = if x > INDENT { 2 } else { 0 };
        if x + sep + width::width(reply) > cw && alone.is_some() && x > INDENT {
            out.push(wrap(&segs));
            segs = vec![seg(Role::Text, " ".repeat(INDENT))];
            (x, sep) = (INDENT, 0);
        }
        if x + sep + width::width(reply) <= cw {
            segs.push(seg(Role::Text, " ".repeat(sep)));
            segs.push(seg(Role::FaintText, reply));
            targets.push(Target {
                row: out.len(),
                x0: 2 + x + sep,
                x1: 2 + x + sep + width::width(reply),
                hit: Hit::Reply(at),
            });
            x += sep + width::width(reply);
        }
    }
    if let Some(tag) = tag_of(card, ctx) {
        let room = cw.saturating_sub(x + 2);
        if room >= 8 {
            segs.push(seg(Role::Text, "  "));
            segs.push(seg(Role::Blocked, cut_dots(&tag, room)));
        }
    }
    out.push(wrap(&segs));
    shown
}

/// The inbox's rows for a box `cols x rows` (border included).
pub fn view(pane: &Pane, ctx: &Ctx, cols: usize, rows: usize) -> View {
    let fade = ctx.fade;
    let cw = cols.saturating_sub(4);
    let inner_rows = rows.saturating_sub(2);
    let mut out: Vec<String> = Vec::new();
    let mut targets = Vec::new();
    let mut cursor = None;
    // The question cut to leave its answers room: the key line names `m`.
    let mut cut = false;
    // The answers of the selected card drawn on its folded row, which the number keys reach.
    let mut reach: Option<usize> = None;

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

    // The card read whole takes every row but the key line, scrolled by `j k`: the words that
    // could not be sent and a failure, what the card is about, the question, then each answer
    // with its reason and what it sends, wrapped, never cut.
    if pane.whole && (pane.open_card().is_some() || !pane.lost.is_empty()) {
        let content = inner_rows.saturating_sub(1);
        let mut lines: Vec<(Vec<Seg>, Option<usize>)> = Vec::new();
        let failed = pane.notice.clone().filter(|_| pane.notice_until.is_none());
        for said in pane.lost_said().chain(failed) {
            for row in wrap_dots(&said, cw, usize::MAX) {
                lines.push((vec![seg(Role::Detail, row)], None));
            }
            lines.push((Vec::new(), None));
        }
        // A text under its label, its rows indented to the label's start.
        let labelled = |lines: &mut Vec<(Vec<Seg>, Option<usize>)>, what: &str, text: &str| {
            lines.push((vec![seg(Role::FaintText, cut_dots(what, cw))], None));
            for row in wrap_dots(text, cw.saturating_sub(2), usize::MAX) {
                lines.push((vec![seg(Role::Detail, format!("  {row}"))], None));
            }
        };
        let card = pane.open_card();
        if let Some(card) = card {
            if let Some(asked) = card.body.asked.as_deref().filter(|t| !t.trim().is_empty()) {
                labelled(&mut lines, "You asked", asked);
            }
            if !card.body.doing.trim().is_empty() {
                labelled(
                    &mut lines,
                    &format!("{} did", card.body.handle),
                    &card.body.doing,
                );
            }
            if show_why(card) {
                labelled(&mut lines, "Why it needs you", &card.body.why);
            }
            lines.push((Vec::new(), None));
            for row in wrap_dots(&card.body.asks, cw, usize::MAX) {
                lines.push((vec![bold(Role::Text, row)], None));
            }
            lines.push((Vec::new(), None));
        }
        let options = card.map_or(&[][..], |c| &c.body.options[..]);
        for (i, o) in options.iter().enumerate() {
            let head = format!("[{}] {}", i + 1, o.label);
            for (n, row) in wrap_dots(&head, cw, usize::MAX).into_iter().enumerate() {
                lines.push((vec![seg(Role::Text, row)], (n == 0).then_some(i)));
            }
            for row in wrap_dots(&o.reason, cw.saturating_sub(4), usize::MAX) {
                lines.push((vec![seg(Role::Detail, format!("    {row}"))], None));
            }
            if let Some(text) = card.and_then(|c| because(c, &o.key)) {
                for row in wrap_dots(
                    &format!("suggested {text}"),
                    cw.saturating_sub(4),
                    usize::MAX,
                ) {
                    lines.push((vec![seg(Role::Done, format!("    {row}"))], None));
                }
            }
            for (n, row) in wrap_dots(&o.words, cw.saturating_sub(11), usize::MAX)
                .into_iter()
                .enumerate()
            {
                let lead = if n == 0 { "    Sends: " } else { "           " };
                lines.push((vec![seg(Role::Detail, format!("{lead}{row}"))], None));
            }
        }
        let end = lines.len().saturating_sub(content);
        pane.whole_end.set(end);
        let first = pane.whole_at.min(end);
        // The answers whose first row is on screen, in order, are the ones `a` to `d` reach.
        let seen = |i: usize| {
            lines
                .iter()
                .position(|(_, at)| *at == Some(i))
                .is_some_and(|row| row >= first && row < first + content)
        };
        let reach = (0..options.len()).take_while(|i| seen(*i)).count();
        for (segs, _) in lines.iter().skip(first).take(content) {
            out.push(wrap(segs));
        }
        while out.len() < 1 + content {
            out.push(blank());
        }
        let keys = if end > 0 {
            "j k scroll  m back"
        } else {
            "m back"
        };
        out.push(wrap(&[seg(Role::FaintText, cut_dots(keys, cw))]));
        out.push(edge(cols, ('\u{2570}', '\u{256f}'), None, None, fade));
        out.truncate(rows);
        pane.shown.set(Some(reach));
        return View {
            rows: out,
            targets: Vec::new(),
            cursor: None,
        };
    }

    // The tabs.
    let n = pane.waiting();
    // On qass's split tab the title folds the column, with the caret a space's fold wears.
    let mut tabs = Vec::new();
    if ctx.board_above {
        tabs.push(bold(Role::Brand, "\u{25be} "));
        targets.push(Target {
            row: out.len(),
            x0: 2,
            x1: 2 + 2 + width::width("questions"),
            hit: Hit::Fold,
        });
    }
    tabs.push(bold(Role::Text, "questions"));
    if n > 0 {
        tabs.push(seg(Role::Text, "  "));
        tabs.push(bold(Role::Blocked, n.to_string()));
    }
    let board_x = width::width("questions") + if n > 0 { 2 + n.to_string().len() } else { 0 } + 2;
    if !ctx.board_above {
        tabs.push(seg(Role::Text, "  "));
        tabs.push(seg(Role::FaintText, "board"));
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
        out.push(wrap(&[seg(Role::Detail, "restart qoop for the questions")]));
    } else if !pane.loaded {
        out.push(wrap(&[seg(Role::FaintText, "reading the questions")]));
    } else if pane.live.is_empty() && !pane.typing {
        out.push(blank());
        out.push(wrap(&[seg(Role::Detail, "Nothing waits for you.")]));
    }

    if let Some(card) = pane.open_card() {
        let sel = pane.selected.min(pane.live.len() - 1);
        let h = folded_h(cw);
        if !pane.is_open() {
            // Every card folded: as many as the rows hold around the selected one, then what he
            // already handled in the rows left.
            let room = inner_rows.saturating_sub(3);
            let n = (room / h).clamp(1, pane.live.len());
            let first = sel
                .saturating_sub((n - 1) / 2)
                .min(pane.live.len().saturating_sub(n));
            for (at, c) in pane.live.iter().enumerate().skip(first).take(n) {
                // A card alone in the pane shows its whole question and every answer.
                let alone = (n == 1).then_some(room);
                let drawn = folded(
                    c,
                    at,
                    at == sel,
                    ctx,
                    cw,
                    alone,
                    &wrap,
                    &mut out,
                    &mut targets,
                );
                if at == sel {
                    reach = Some(drawn);
                }
            }
            let left = room.saturating_sub(n * h);
            if left >= 4 && !pane.ended.is_empty() {
                out.push(blank());
                out.push(wrap(&[
                    seg(Role::FaintText, "handled for you "),
                    seg(Role::Detail, pane.ended.len().to_string()),
                ]));
                out.push(hr());
                for c in pane.ended.iter().take(left - 3) {
                    out.push(handled_row(c, ctx, cw, &wrap));
                }
            }
        } else {
            let label_w = card
                .body
                .options
                .iter()
                .map(|o| width::width(&o.label))
                .max()
                .unwrap_or(0)
                .min(cw / 3);
            let reason_room = cw.saturating_sub(4 + label_w + 2);
            let stacked = cw < LABEL_W;
            let text_room = if stacked { cw - 2 } else { cw - LABEL_COL };
            let side_rows = |text: &str, room: usize| wrap_dots(text, room, SIDE_ROWS).len();
            let asked = card.body.asked.as_deref().filter(|t| !t.trim().is_empty());
            let doing = Some(card.body.doing.as_str()).filter(|t| !t.trim().is_empty());
            let why = show_why(card).then_some(card.body.why.as_str());
            let lens = Lens {
                ask: wrap_dots(&card.body.asks, cw, usize::MAX).len(),
                asked: asked.map_or(0, |t| side_rows(t, text_room)),
                doing: doing.map_or(0, |t| side_rows(t, text_room)),
                why: why.map_or(0, |t| side_rows(t, text_room)),
                reasons: card
                    .body
                    .options
                    .iter()
                    .map(|o| side_rows(&o.reason, reason_room))
                    .collect(),
                becauses: card
                    .body
                    .options
                    .iter()
                    .map(|o| {
                        because(card, &o.key).map_or(0, |t| side_rows(&t, cw.saturating_sub(14)))
                    })
                    .collect(),
                sends: card
                    .body
                    .options
                    .iter()
                    .map(|o| side_rows(&o.words, cw.saturating_sub(11)))
                    .collect(),
                tag: tag_of(card, ctx).is_some() || card.item.is_some(),
                stacked,
            };
            let b = budget(
                inner_rows,
                pane.live.len() - 1,
                h,
                pane.typing || !pane.text.is_empty(),
                &lens,
            );
            let side = b.side;
            // The other cards stay folded around the open one: before it first.
            let below_all = pane.live.len() - 1 - sel;
            let mut above = sel.min(b.others.div_ceil(2));
            let below = below_all.min(b.others - above);
            above = sel.min(b.others - below);
            let folded_at = |at: usize, out: &mut Vec<String>, targets: &mut Vec<Target>| {
                folded(
                    &pane.live[at],
                    at,
                    false,
                    ctx,
                    cw,
                    None,
                    &wrap,
                    out,
                    targets,
                );
            };
            for at in sel - above..sel {
                folded_at(at, &mut out, &mut targets);
            }

            // The open card.
            // Who asks reads first and whole (to 24 columns); then the wait, short when it must
            // be, and the space in what is left, cut first.
            // A plan card says what it asks: the item line under it names whose plan.
            let name = if plan_card(card) {
                "Approve the plan".to_string()
            } else {
                name_of(card, ctx)
            };
            let title_left = format!("\u{25be} {}", cut_dots(&name, cw.saturating_sub(2).min(24)));
            let room = cw.saturating_sub(width::width(&title_left) + 2);
            let age = ctx.now_unix - card.opened_at;
            let waited = format!("waiting {}", age_long(age));
            let right = match space_of(card, ctx) {
                Some(space) if room >= width::width(&waited) + 3 + 4 => format!(
                    "{} \u{b7} {waited}",
                    cut_dots(&space, room - width::width(&waited) - 3)
                ),
                _ if room >= width::width(&waited) => waited,
                _ if room >= age_short(age).len() => age_short(age),
                _ => String::new(),
            };
            let gap = cw.saturating_sub(width::width(&title_left) + width::width(&right));
            targets.push(Target {
                row: out.len(),
                x0: 2,
                x1: 2 + cw,
                hit: Hit::Card(sel),
            });
            out.push(wrap(&[
                bold(Role::Text, title_left),
                seg(Role::Text, " ".repeat(gap)),
                seg(Role::FaintText, right),
            ]));
            // The item it is about, then the tag, on the row under the heading.
            let mut under = item_line(card).unwrap_or_default();
            if let Some(tag) = tag_of(card, ctx) {
                if !under.is_empty() {
                    under.push(seg(Role::Text, "  "));
                }
                under.push(seg(Role::Blocked, tag));
            }
            match under.is_empty() {
                false => out.push(wrap(&under)),
                true if b.head_gap => out.push(blank()),
                true => {}
            }
            // A text that did not fit its rows is cut: the key line names `m`.
            let clipped = std::cell::Cell::new(false);
            // A text on rows after its lead (a label, a key), the later rows indented under the
            // text.
            let rows_after = |lead: Vec<Seg>, indent: usize, text: &str| -> Vec<String> {
                let room = cw.saturating_sub(indent);
                if wrap_dots(text, room, usize::MAX).len() > side {
                    clipped.set(true);
                }
                let mut lead = Some(lead);
                wrap_dots(text, room, side)
                    .into_iter()
                    .map(|t| {
                        let mut segs = lead
                            .take()
                            .unwrap_or_else(|| vec![seg(Role::Text, " ".repeat(indent))]);
                        segs.push(seg(Role::Detail, t));
                        wrap(&segs)
                    })
                    .collect()
            };
            // The thread's parts: its label, in a column left of the text, or over it in a
            // narrow pane.
            let said = |what: &str, text: &str| -> Vec<String> {
                if stacked {
                    let mut rows = vec![wrap(&[seg(Role::FaintText, cut_dots(what, cw))])];
                    rows.extend(rows_after(vec![seg(Role::Text, "  ")], 2, text));
                    rows
                } else {
                    let lead = width::fit(&cut_dots(what, LABEL_COL - 2), LABEL_COL);
                    rows_after(vec![seg(Role::FaintText, lead)], LABEL_COL, text)
                }
            };
            if b.asked
                && let Some(text) = asked
            {
                out.extend(said("You asked", text));
            }
            if b.doing
                && let Some(text) = doing
            {
                out.extend(said(&format!("{} did", card.body.handle), text));
            }
            if b.why
                && let Some(text) = why
            {
                out.extend(said("Why it needs you", text));
            }
            if b.gap_top {
                out.push(blank());
            }
            cut = b.ask < lens.ask;
            for row in wrap_dots(&card.body.asks, cw, b.ask) {
                out.push(wrap(&[bold(Role::Text, row)]));
            }
            let passed = card.status != "open";
            for (i, o) in card.body.options.iter().enumerate() {
                let first = out.len();
                // An answer cut to its room is read whole with `m`, as the question is.
                cut |= width::width(&o.label) > label_w;
                let key_ink = if passed { Role::FaintText } else { Role::Brand };
                let lab = width::fit(&cut_dots(&o.label, label_w), label_w);
                let lead = vec![
                    seg(key_ink, format!("[{}] ", i + 1)),
                    seg(if passed { Role::FaintText } else { Role::Text }, lab),
                    seg(Role::Text, "  "),
                ];
                out.extend(rows_after(lead, 4 + label_w + 2, &o.reason));
                if b.because
                    && let Some(text) = because(card, &o.key)
                {
                    let lead = vec![
                        seg(Role::Text, "    "),
                        seg(
                            if passed { Role::FaintText } else { Role::Done },
                            "suggested ",
                        ),
                    ];
                    out.extend(rows_after(lead, 14, &text));
                }
                if b.sends {
                    let lead = vec![seg(Role::Text, "    "), seg(Role::FaintText, "Sends: ")];
                    out.extend(rows_after(lead, 11, &o.words));
                }
                for row in first..out.len() {
                    targets.push(Target {
                        row,
                        x0: 2,
                        x1: 2 + cw,
                        hit: Hit::Option(i),
                    });
                }
            }
            cut |= clipped.get() || !b.sends || !b.because && lens.becauses.iter().any(|&n| n > 0);
            if b.gap_box {
                out.push(blank());
            }
            // The one text box.
            if b.text_box {
                targets.push(Target {
                    row: out.len() + 1,
                    x0: 2,
                    x1: 2 + cw,
                    hit: Hit::Box,
                });
                let (rows, caret) = text_box(pane, cw, passed, &wrap);
                cursor = caret.map(|col| (out.len() + 1, col)).or(cursor);
                out.extend(rows);
            }
            for at in sel + 1..sel + 1 + below {
                folded_at(at, &mut out, &mut targets);
            }

            // What he already handled.
            if b.handled > 0 && !pane.ended.is_empty() {
                out.push(blank());
                out.push(wrap(&[
                    seg(Role::FaintText, "handled for you "),
                    seg(Role::Detail, pane.ended.len().to_string()),
                ]));
                out.push(hr());
                for c in pane.ended.iter().take(b.handled.saturating_sub(2)) {
                    out.push(handled_row(c, ctx, cw, &wrap));
                }
            }
        }
    } else if pane.loaded && !pane.unsupported {
        // Nothing waits: his words while he types (their card went elsewhere), and what was
        // handled.
        out.push(blank());
        if pane.typing {
            let (rows, caret) = text_box(pane, cw, false, &wrap);
            cursor = caret.map(|col| (out.len() + 1, col));
            out.extend(rows);
            out.push(blank());
        }
        if !pane.ended.is_empty() {
            out.push(wrap(&[
                seg(Role::FaintText, "handled for you "),
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
    // said, or the keys. Words that could not be sent, then a failure, are wrapped whole in as
    // many rows as they need, so their words can be read and copied (and `m` reads them whole
    // when even that is short).
    let standing: Vec<String> = pane
        .lost_said()
        .chain(pane.notice.clone().filter(|_| pane.notice_until.is_none()))
        .collect();
    let joined = standing.join("; ");
    let most = inner_rows.saturating_sub(2).max(1);
    let mut said = wrap_dots(&joined, cw, usize::MAX);
    if pane.typing {
        // The box he types in keeps its rows and its caret: what stands yields to one row,
        // read whole with `m` once esc ends the typing.
        said = wrap_dots(&joined, cw, 1);
    } else if said.len() > most {
        // More than the room: as much as fits, and the key that reads it all.
        said = wrap_dots(&joined, cw, most.saturating_sub(1).max(1));
        said.push(cut_dots("m reads it whole", cw));
    }
    let mut foot: Vec<String> = said
        .into_iter()
        .filter(|_| !standing.is_empty())
        .map(|r| wrap(&[seg(Role::Detail, r)]))
        .collect();
    // While he types for a card handled elsewhere, the line that says so closes the foot.
    if pane.typing && pane.orphaned() {
        for row in elsewhere_rows(pane, cw) {
            foot.push(wrap(&[seg(Role::Detail, row)]));
        }
    } else if foot.is_empty() {
        foot.push(wrap(&bottom_line(pane, cw, cut)));
    }
    let content = inner_rows.saturating_sub(foot.len());
    out.truncate(1 + content);
    while out.len() < 1 + content {
        out.push(blank());
    }
    out.extend(foot);
    out.push(edge(cols, ('\u{2570}', '\u{256f}'), None, None, fade));
    out.truncate(rows);
    // A target below the clipped rows is no target, and an answer not drawn takes no key.
    targets.retain(|t| t.row <= content);
    let mut shown: Vec<usize> = targets
        .iter()
        .filter_map(|t| match t.hit {
            Hit::Option(i) => Some(i),
            _ => None,
        })
        .collect();
    shown.dedup();
    // On a folded card, the answers still in sight after the clip, in order: a number key
    // never sends one the pane had no room to draw.
    let sel = pane.selected.min(pane.live.len().saturating_sub(1));
    let reach = reach.map(|_| {
        (0..)
            .take_while(|i| targets.iter().any(|t| t.hit == Hit::Answer(sel, *i)))
            .count()
    });
    pane.shown.set(Some(reach.unwrap_or(shown.len())));
    View {
        rows: out,
        targets,
        cursor: cursor.filter(|(r, _)| *r <= content),
    }
}

/// The line a W card carries when the daemon knew nothing of why: the card says who waits
/// instead, so the label is left out.
const GENERIC_WHY: &str = "it stopped and waits for your answer";

/// Whether "Why it needs you" is worth a row: not for an empty one, and not for the generic line
/// a W card (the agent's own question, drafted by the daemon) carries.
fn show_why(card: &AskInfo) -> bool {
    let why = card.body.why.trim();
    !why.is_empty()
        && !(card.card.starts_with('W')
            && why.trim_end_matches('.').eq_ignore_ascii_case(GENERIC_WHY))
}

/// Why option `key` is the suggested one, as `because ...`; none when it is not the pick.
fn because(card: &AskInfo, key: &str) -> Option<String> {
    let pick = card.body.pick.as_ref().filter(|p| p.key == key)?;
    let reason = pick.reason.trim();
    let reason = reason
        .get(..8)
        .filter(|head| head.eq_ignore_ascii_case("because "))
        .map_or(reason, |_| reason[8..].trim_start());
    Some(format!("because {reason}"))
}

/// The one text box, as its three rows, and the caret's column in its middle row while he
/// types.
fn text_box(
    pane: &Pane,
    cw: usize,
    passed: bool,
    wrap: &impl Fn(&[Seg]) -> String,
) -> ([String; 3], Option<usize>) {
    let box_w = cw;
    // Cut to the box: the column may be dragged as narrow as the questions go.
    let title = cut_dots(" reply in your own words ", box_w.saturating_sub(4));
    let mut top = format!("\u{256d}\u{2500}{title}");
    top.push_str(&"\u{2500}".repeat(box_w.saturating_sub(width::width(&top) + 1)));
    top.push('\u{256e}');
    let box_ink = if pane.typing {
        Role::Focused
    } else {
        Role::Rule
    };
    let top = wrap(&[seg(box_ink, top)]);
    let inside = box_w.saturating_sub(4);
    let shown_text = if pane.typing {
        // The tail of what is typed, by cells (a wide letter takes two), with room for the
        // caret.
        let mut room = inside.saturating_sub(1);
        let mut tail: Vec<char> = pane
            .text
            .chars()
            .rev()
            .take_while(|c| {
                let fits = width::columns(*c) <= room;
                room = room.saturating_sub(width::columns(*c));
                fits
            })
            .collect();
        tail.reverse();
        format!("{}\u{2588}", tail.into_iter().collect::<String>())
    } else if passed {
        "waiting for qass".to_string()
    } else if !pane.text.is_empty() {
        // The answer typed and kept, faint until `r` takes it up again.
        cut_dots(&pane.text, inside)
    } else {
        "r to reply".to_string()
    };
    let text_role = if pane.typing {
        Role::Text
    } else {
        Role::FaintText
    };
    let caret = pane
        .typing
        .then(|| 2 + 2 + width::width(&shown_text).saturating_sub(1));
    let middle = wrap(&[
        seg(box_ink, "\u{2502} "),
        seg(text_role, width::fit(&shown_text, inside)),
        seg(box_ink, " \u{2502}"),
    ]);
    let bottom = wrap(&[seg(
        box_ink,
        format!(
            "\u{2570}{}\u{256f}",
            "\u{2500}".repeat(box_w.saturating_sub(2))
        ),
    )]);
    ([top, middle, bottom], caret)
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
    let name_w = 12.min(cw / 4);
    let name = width::fit(
        &cut_dots(&sidebar::chip_name(&name_of(c, ctx)), name_w),
        name_w,
    );
    let right = format!("{result}  {age}");
    let room = cw.saturating_sub(width::width(&name) + 2 + width::width(&right) + 1);
    let asks = cut_dots(&c.body.asks, room);
    let gap =
        cw.saturating_sub(width::width(&name) + 2 + width::width(&asks) + width::width(&right));
    wrap(&[
        seg(Role::FaintText, name),
        seg(Role::FaintText, "  "),
        seg(Role::FaintText, asks),
        seg(Role::Text, " ".repeat(gap)),
        seg(Role::FaintText, right),
    ])
}

/// The line that asks qass to talk `card` through, his words to follow. It never starts with
/// the handle or `E<n> B`, which qass reads as an answer. The daemon refuses card values with a
/// control character, so it stays one line.
pub fn talk_line(card: &AskInfo) -> String {
    format!(
        "About {} ({}): {} ",
        card.card, card.body.handle, card.body.asks
    )
}

/// Too short or too common to name an agent.
const COMMON: &[&str] = &[
    "about", "also", "been", "claude", "codex", "coding", "could", "from", "have", "into", "just",
    "need", "shall", "should", "some", "than", "that", "their", "them", "then", "there", "these",
    "they", "this", "what", "when", "which", "will", "with", "would", "your",
];

/// `T5` is a tab on this machine, `devbox/T5` a tab on the remote called devbox.
fn parse_ref(token: &str) -> Option<(Option<&str>, u32)> {
    let (remote, tab) = match token.rsplit_once('/') {
        Some((remote, tab)) => (Some(remote), tab),
        None => (None, token),
    };
    let n = tab.strip_prefix('T')?;
    n.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| n.parse().ok())
        .flatten()
        .map(|n| (remote, n))
}

/// The tab of that reference among `rows`: a remote tab is told by its number in its own
/// daemon and by the remote's name, which starts its space's name (`devbox:<space>`).
fn tab_of(
    (remote, n): (Option<&str>, u32),
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    qass: Option<i64>,
) -> Option<u32> {
    rows.iter()
        .find(|r| {
            r.agent
                && match remote {
                    None => {
                        !crate::mux::is_remote_terminal(r.terminal)
                            && r.terminal == n
                            && Some(r.workspace) != qass
                    }
                    Some(name) => {
                        crate::mux::is_remote_terminal(r.terminal)
                            && crate::mux::daemon_terminal(r.terminal) == n
                            && spaces
                                .iter()
                                .find(|s| s.id == r.workspace)
                                .is_some_and(|s| {
                                    s.name
                                        .split(':')
                                        .next()
                                        .is_some_and(|p| p.eq_ignore_ascii_case(name))
                                })
                    }
                }
        })
        .map(|r| r.terminal)
}

fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 4 && !COMMON.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// The one agent whose name or folder shares a word with `said` (one starts the other, so a
/// handle like `shoppay` finds the `shop` folder). Names and folders are taken whole: a
/// branch-like name must not match on its everyday words, and titles and goals are left out
/// for the same reason. A word that fits more than three agents names none of them; the agent
/// with most shared words wins, then the one active last.
fn named(said: &str, rows: &[&AgentRow]) -> Option<u32> {
    let ids: Vec<(&AgentRow, Vec<String>)> = rows
        .iter()
        .map(|r| {
            let folder = r
                .cwd
                .as_deref()
                .and_then(|c| c.rsplit('/').find(|s| !s.is_empty()));
            let ids = [r.name.as_deref(), folder]
                .into_iter()
                .flatten()
                .map(str::to_lowercase)
                .filter(|x| x.chars().count() >= 4 && !COMMON.contains(&x.as_str()))
                .collect();
            (*r, ids)
        })
        .collect();
    let mut score: Vec<(&AgentRow, usize)> = Vec::new();
    for a in words(said) {
        let fits: Vec<&AgentRow> = ids
            .iter()
            .filter(|(_, w)| {
                w.iter()
                    .any(|b| a.starts_with(b.as_str()) || b.starts_with(&a))
            })
            .map(|(r, _)| *r)
            .collect();
        if fits.len() > 3 {
            continue;
        }
        for r in fits {
            match score.iter_mut().find(|(s, _)| std::ptr::eq(*s, r)) {
                Some(hit) => hit.1 += 1,
                None => score.push((r, 1)),
            }
        }
    }
    // A full tie goes to the first row, as on the phone.
    score
        .into_iter()
        .enumerate()
        .max_by_key(|(i, (r, n))| {
            (
                *n,
                r.last_active_at_unix.unwrap_or(0),
                std::cmp::Reverse(*i),
            )
        })
        .map(|(_, (r, _))| r.terminal)
}

/// The tab Talk it through opens for `card`: the agent it is about, `None` for a card about
/// nothing, which goes to qass. The first hit wins: the card's terminal; its `T5` or
/// `devbox/T5` agent; such a reference in its words; the one agent whose name or folder its
/// words name. The phone's `talkRoute` (mobile/src/ui/asks.ts) follows the same order.
pub fn talk_target(card: &AskInfo, rows: &[AgentRow], spaces: &[WorkspaceRow]) -> Option<u32> {
    let b = &card.body;
    let said = [
        b.handle.as_str(),
        b.asked.as_deref().unwrap_or(""),
        &b.doing,
        &b.why,
        &b.asks,
    ]
    .join(" ");
    let qass = sidebar::qass_space(spaces);
    let workers: Vec<&AgentRow> = rows
        .iter()
        .filter(|r| {
            r.agent && !crate::mux::is_remote_terminal(r.terminal) && Some(r.workspace) != qass
        })
        .collect();
    let by_agent = || {
        card.agent
            .as_deref()
            .and_then(parse_ref)
            .and_then(|r| tab_of(r, rows, spaces, qass))
    };
    let by_task = || {
        let id = card.task?;
        let hit = workers
            .iter()
            .find(|r| r.task.as_ref().is_some_and(|t| t.id == id));
        hit.map(|r| r.terminal)
    };
    let by_words = || {
        let tokens =
            said.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '-' || c == '_'));
        // `T24-worker` holds T24: a token that is no reference is tried by its `-` pieces.
        let mut refs = tokens.flat_map(|t| {
            parse_ref(t)
                .into_iter()
                .chain(t.split('-').filter_map(parse_ref))
        });
        refs.find_map(|r| tab_of(r, rows, spaces, qass))
    };
    // A terminal that is gone (a stale card) is not a tab to open; a draft keeps its tab.
    let own = card
        .terminal
        .filter(|t| card.card.starts_with('W') || workers.iter().any(|r| r.terminal == *t));
    own.or_else(by_agent)
        .or_else(by_task)
        .or_else(by_words)
        .or_else(|| named(&said, &workers))
}

/// The rows under a box whose card was handled elsewhere: that first, whose card the words
/// were for, the keys last, in the longest form that fits one row, else the shortest wrapped
/// on words, never a word cut.
fn elsewhere_rows(pane: &Pane, cw: usize) -> Vec<String> {
    let whose = match pane.typing_for.as_ref().map_or("", |(_, handle)| handle) {
        "" => String::new(),
        who => format!(": {who}'s card"),
    };
    let short = format!("{HANDLED_ELSEWHERE}{whose}; esc stops typing");
    [
        format!("{HANDLED_ELSEWHERE}{whose}; enter sends nothing, esc stops typing"),
        short.clone(),
    ]
    .into_iter()
    .find(|line| width::width(line) <= cw)
    .map_or_else(|| wrap_dots(&short, cw, usize::MAX), |line| vec![line])
}

/// The longest of `lines` that fits `cw` columns, else the last.
fn pick_keys(cw: usize, lines: &[&str]) -> String {
    let fits = lines.iter().find(|l| width::width(l) <= cw);
    fits.or(lines.last())
        .map_or_else(String::new, |l| l.to_string())
}

fn bottom_line(pane: &Pane, cw: usize, cut: bool) -> Vec<Seg> {
    // For as long as the words stay, never back to `enter send`; named, since another card
    // may stand over them.
    // A failure stands over everything: it is the one thing that cannot be read again.
    if let Some(n) = pane.notice.as_ref().filter(|_| pane.notice_until.is_none()) {
        return vec![seg(Role::Detail, cut_dots(n, cw))];
    }
    if let Some(n) = &pane.notice {
        return vec![seg(Role::Detail, cut_dots(n, cw))];
    }
    // An answer on its way says so until the daemon takes it.
    if let Some((handle, _)) = pane.sent.values().next() {
        return vec![seg(
            Role::Detail,
            cut_dots(&format!("sending to {handle}\u{2026}"), cw),
        )];
    }
    let keys = if pane.typing {
        "enter send  esc stop typing".to_string()
    } else if !pane.focus {
        "ctrl-a i focuses this".to_string()
    } else if cut {
        pick_keys(
            cw,
            &[
                "m read whole  1-9 answer  r reply  j k cards",
                "m read whole  1-9 answer  r reply",
            ],
        )
    } else {
        let enter = if pane.is_open() {
            "enter fold"
        } else {
            "enter open"
        };
        pick_keys(
            cw,
            &[
                &format!(
                    "1-9 answer  {enter}  r reply  g go to tab  o talk it through  x put away  j k move"
                ),
                &format!("1-9 answer  {enter}  r reply  g tab  o talk  x away  j k move"),
                &format!("1-9 answer  {enter}  r reply  j k move"),
            ],
        )
    };
    vec![seg(Role::FaintText, cut_dots(&keys, cw))]
}

/// `text` wrapped on words into rows of `cols` columns, at most `max` of them; a word longer than a
/// row breaks on a character, and when text remains the last row ends in `\u{2026}`. With one row
/// it is [`cut_dots`].
fn wrap_dots(text: &str, cols: usize, max: usize) -> Vec<String> {
    if max <= 1 || width::width(text) <= cols {
        return vec![cut_dots(text, cols)];
    }
    let mut rows = vec![String::new()];
    let mut used = 0;
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().filter(|c| width::drawn(*c)).collect();
        let w: usize = chars.iter().map(|c| width::columns(*c)).sum();
        if w == 0 {
            continue;
        }
        if used > 0 && used + 1 + w <= cols {
            if let Some(r) = rows.last_mut() {
                r.push(' ');
            }
            used += 1;
        } else if used > 0 {
            rows.push(String::new());
            used = 0;
        }
        for c in chars {
            let cw = width::columns(c);
            if cw > cols {
                continue;
            }
            if used + cw > cols {
                rows.push(String::new());
                used = 0;
            }
            if let Some(r) = rows.last_mut() {
                r.push(c);
            }
            used += cw;
        }
    }
    if rows.len() > max {
        rows.truncate(max);
        if let Some(last) = rows.last_mut() {
            *last = cut_dots(&format!("{last}\u{2026}"), cols);
        }
    }
    rows
}

/// `text` cut to `cols` columns with a closing `\u{2026}` when it did not fit, never after a
/// space.
pub(crate) fn cut_dots(text: &str, cols: usize) -> String {
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
    let mut out = out.trim_end().to_string();
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
            item: None,
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
            command: None,
            clients: Vec::new(),
            typed_by: None,
            parked: false,
            keep_awake: false,
            qass_skip: false,
            background: 0,
            subagents: 0,
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
            stop: None,
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
        // Most tests read the card in full: they open it first (a folded one has its own).
        p.expanded = true;
        p
    }

    fn ctx<'a>(rows: &'a [AgentRow], spaces: &'a [WorkspaceRow]) -> Ctx<'a> {
        Ctx {
            rows,
            spaces,
            now_unix: 1_000,
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

    /// A card about a board item names it under its heading, with its phase and its plan; a
    /// plan card not approved yet reads "Approve the plan" and keeps its answers. A card with no
    /// item (an older daemon, another machine) draws as before, with its agent's name.
    #[test]
    fn a_card_names_its_item_and_a_plan_card_asks_for_the_plan() {
        use qoop_runtime::control::{AskItem, PhaseInfo};
        let item = |plan: &str| AskItem {
            id: 7,
            key: "QOOP-58".into(),
            plan: plan.into(),
            phase: Some(PhaseInfo {
                n: 2,
                of: 3,
                name: "Build".into(),
            }),
        };
        let mut p = Pane::default();
        let mut plan = card(1, "plan", "open");
        plan.item = Some(item("written"));
        p.take(vec![plan], Vec::new());
        p.expanded = true;
        let t = text(&view(&p, &ctx(&[], &[]), 64, 36));
        assert!(t.contains("\u{25be} Approve the plan"), "{t}");
        assert!(
            t.contains("QOOP-58 \u{b7} Build 2/3 \u{b7} plan needs you"),
            "{t}"
        );
        assert!(t.contains("[1] run now"), "answered as any card: {t}");

        // Approved, or a card of another kind: the agent's name heads it, the plan as it is.
        let mut done = card(1, "plan", "open");
        done.item = Some(item("approved"));
        p.take(vec![done], Vec::new());
        p.expanded = true;
        let t = text(&view(&p, &ctx(&[], &[]), 64, 36));
        assert!(!t.contains("Approve the plan"), "{t}");
        assert!(t.contains("\u{25be} plan") && t.contains("plan ok"), "{t}");
        let mut asked = card(1, "migrations", "open");
        asked.item = Some(item("stale"));
        p.take(vec![asked], Vec::new());
        p.expanded = true;
        let t = text(&view(&p, &ctx(&[], &[]), 64, 36));
        assert!(t.contains("\u{25be} migrations"), "{t}");
        assert!(
            t.contains("QOOP-58 \u{b7} Build 2/3 \u{b7} plan needs you"),
            "{t}"
        );

        // No item: no line, the heading as before.
        p.take(vec![card(1, "plan", "open")], Vec::new());
        p.expanded = true;
        let t = text(&view(&p, &ctx(&[], &[]), 64, 36));
        assert!(!t.contains("QOOP-58") && !t.contains("Approve"), "{t}");
        assert_eq!(plan_word("", None), None);
        assert_eq!(
            plan_word("approved", Some("owner")).map(|w| w.0),
            Some("plan ok \u{b7} you".to_string())
        );
    }

    #[test]
    fn three_cards_show_the_folded_cards_the_open_card_and_the_text_box() {
        let p = pane(3);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 40));
        for want in [
            "questions  3  board",
            "web-app",
            "api-server",
            "Run 0042 on staging now?",
            "[1] run now",
            "[2] wait for you (suggested)",
            "Sends: Do not run 0042 yet",
            "suggested because a column drop",
            "reply in your own words",
            "r to reply",
            "waiting 6 min",
            "You asked",
            "migrations did",
            "Why it needs you",
        ] {
            assert!(t.contains(want), "{want:?} in\n{t}");
        }
        // Each option with its reason and what it sends, in this order after the question.
        let at = |w: &str| t.find(w).unwrap_or_else(|| panic!("{w} in {t}"));
        assert!(
            at("You asked") < at("migrations did")
                && at("migrations did") < at("Why it needs you")
                && at("Why it needs you") < at("Run 0042 on staging now?")
                && at("no rollback exists") < at("Sends: Run 0042")
                && at("Sends: Do not run") < at("reply in your own words"),
            "{t}"
        );
    }

    /// A long question alone in a short pane pushes answers past its foot: a number key sends
    /// only an answer the pane drew.
    #[test]
    fn a_number_key_never_sends_an_answer_cut_off_the_pane() {
        let mut long = card(1, "migrations", "open");
        long.body.asks = "Shall I run this migration now? "
            .repeat(18)
            .trim()
            .to_string();
        let mut p = Pane::default();
        p.take(vec![long], Vec::new());
        p.focus = true;
        let t = text(&view(&p, &ctx(&[], &[]), 50, 14));
        assert!(!t.contains("[3]"), "the third answer is off the pane:\n{t}");
        p.key(b"3", 1);
        assert!(p.outbox.is_empty(), "no key for an answer not drawn");
        let drawn = (1..=3).filter(|n| t.contains(&format!("[{n}]"))).count();
        if drawn > 0 {
            p.key(format!("{drawn}").as_bytes(), 1);
            assert_eq!(p.outbox.len(), 1, "a drawn answer still takes its key");
        }
    }

    /// Folded, a card is its question and its numbered answers on two rows, the suggested one
    /// marked, with the keys named; enter opens the selected one in place and folds it again.
    #[test]
    fn folded_cards_show_the_question_and_numbered_answers_and_enter_opens() {
        let mut p = Pane::default();
        p.take(
            vec![
                card(1, "migrations", "open"),
                card(2, "web-app", "open"),
                card(3, "api-server", "open"),
            ],
            Vec::new(),
        );
        p.focus = true;
        for cols in [120, 100, 64] {
            let t = text(&view(&p, &ctx(&[], &[]), cols, 40));
            let answers = if cols > 70 {
                "[1] run now  [2] wait for you (suggested)  [3] skip  [r] Reply"
            } else {
                "[1] run now  [2] wait for you (suggested)  [3] skip"
            };
            for want in [
                "migrations",
                "web-app",
                "api-server",
                "later",
                "Run 0042 on staging now?",
                answers,
                "1-9 answer  enter open  r reply",
            ] {
                assert!(t.contains(want), "{want:?} at {cols}:\n{t}");
            }
            assert!(
                !t.contains("Sends:") && !t.contains("You asked"),
                "folded at {cols}:\n{t}"
            );
        }
        // At 120 columns the question and the place share the head row.
        let t = text(&view(&p, &ctx(&[], &[]), 120, 40));
        assert!(
            t.contains("migrations") && t.contains("later   6m  Run 0042 on staging now?"),
            "{t}"
        );
        // A one-key answer on a folded card sends the option it names.
        p.key(b"2", 1);
        assert_eq!(p.outbox.len(), 1, "digit answers the numbered option");
        p.outbox.clear();
        p.sent.clear();
        // Enter opens, enter folds.
        assert!(!p.is_open());
        p.key(b"\r", 1);
        assert!(p.is_open());
        let t = text(&view(&p, &ctx(&[], &[]), 100, 40));
        assert!(t.contains("Sends:") && t.contains("enter fold"), "{t}");
        p.key(b"\r", 1);
        assert!(!p.is_open());
        // Moving to another card leaves it folded.
        p.key(b"\r", 1);
        p.key(b"j", 1);
        assert!(!p.is_open());
    }

    /// A W card with no options, a generic `why` and nothing asked: no empty label, `Reply` is its
    /// only action, and the open card does not say the generic line.
    #[test]
    fn a_card_without_options_or_a_real_why_hides_what_it_has_not_got() {
        let mut w = card(7, "dub52", "open");
        w.card = "W7".into();
        w.body.options.clear();
        w.body.pick = None;
        w.body.asked = None;
        w.body.why = "it stopped and waits for your answer".into();
        let mut p = Pane::default();
        p.take(vec![w.clone()], Vec::new());
        let t = text(&view(&p, &ctx(&[], &[]), 80, 30));
        assert!(t.contains("[r] Reply") && !t.contains("[1]"), "{t}");
        p.key(b"\r", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 80, 30));
        assert!(
            t.contains("dub52 did") && t.contains("Run 0042 on staging now?"),
            "{t}"
        );
        assert!(
            !t.contains("Why it needs you")
                && !t.contains("You asked")
                && !t.contains("waits for your answer"),
            "{t}"
        );
        // The same line on an E card is qass's own words: shown. A real why on a W card too.
        let mut e = w.clone();
        e.card = "E7".into();
        p.take(vec![e], Vec::new());
        p.expanded = true;
        assert!(text(&view(&p, &ctx(&[], &[]), 80, 30)).contains("Why it needs you"));
        let mut real = w;
        real.body.why = "the deploy waits on his word".into();
        p.take(vec![real], Vec::new());
        p.expanded = true;
        assert!(text(&view(&p, &ctx(&[], &[]), 80, 30)).contains("the deploy waits on his word"));
    }

    /// An answer typed and left with `esc` stays in the box, faint, until `t` takes it up.
    #[test]
    fn a_kept_answer_shows_in_the_box_when_not_typing() {
        let mut p = pane(3);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"run it after the backup", 1);
        p.key(b"\x1b", 1);
        assert!(!p.typing());
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("run it after the backup") && !t.contains("r to reply"),
            "{t}"
        );
    }

    /// Words typed for a card wait for it while another card is open (by key, by click or
    /// by a fresh read), come back with it, and go only when that card is answered.
    #[test]
    fn a_typed_answer_waits_for_its_card_and_goes_when_sent() {
        let mut p = pane(3);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"run it after the backup", 1);
        p.key(b"\x1b", 1);
        p.key(b"j", 1);
        assert_eq!(p.typed(), "", "the second card has no words of its own");
        p.key(b"r", 1);
        p.key(b"skip this one", 1);
        p.click(Hit::Card(0));
        assert_eq!(p.typed(), "run it after the backup");
        // Kept words show in the box however short the pane: the box stays while it holds them.
        for rows in 14..=20 {
            let t = text(&view(&p, &ctx(&[], &[]), 64, rows));
            assert!(t.contains("run it after the backup"), "{rows} rows:\n{t}");
        }
        p.take(p.live.clone(), Vec::new());
        assert_eq!(p.typed(), "run it after the backup", "a read keeps them");
        p.key(b"r", 1);
        p.key(b"\r", 1);
        assert_eq!(p.outbox.len(), 1, "sent");
        p.key(b"j", 1);
        assert_eq!(p.typed(), "skip this one");
        p.key(b"k", 1);
        assert_eq!(p.typed(), "", "the sent card's words are gone");
    }

    /// A read that takes away the card words were kept for leaves the next card's box empty
    /// (nothing of them can be sent to another agent); a read that only moves that card keeps
    /// them with it.
    #[test]
    fn a_read_keeps_words_with_their_card() {
        let mut p = pane(3);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"for the first", 1);
        p.key(b"\x1b", 1);
        let moved = vec![p.live[1].clone(), p.live[0].clone(), p.live[2].clone()];
        p.take(moved, Vec::new());
        assert_eq!(
            p.typed(),
            "for the first",
            "moved, the card keeps its words"
        );
        let rest = vec![p.live[0].clone(), p.live[2].clone()];
        p.take(rest, Vec::new());
        assert_eq!(p.typed(), "", "gone, its words go with it");
        p.key(b"r", 1);
        p.key(b"\r", 1);
        assert!(p.outbox.is_empty(), "nothing sent to the card in its place");
    }

    /// A write that fails for a card ended elsewhere while the person types: the failure stands
    /// over the "handled elsewhere" line, holds the lost words, and outlasts the typing keys.
    #[test]
    fn a_failure_for_a_card_ended_elsewhere_shows_its_lost_words() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"land 42 first", 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        p.key(b"r", 1);
        p.key(b"and", 1);
        p.take(p.live[1..].to_vec(), Vec::new());
        p.answered(&ControlResponse::Failed {
            id,
            reason: "closed".into(),
        });
        p.key(b" more", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 90, 36));
        assert!(
            t.contains("not sent, migrations's card ended: land 42 first; not sent: closed"),
            "{t}"
        );
    }

    /// The daemon's own order: the failure first, then the read without the card. The words put
    /// back for a retry do not stay in the box over the next card, and are not lost unseen:
    /// they stand in a notice with whose card ended.
    #[test]
    fn words_put_back_for_a_card_that_then_ends_stand_in_a_notice() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"land it", 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        p.answered(&ControlResponse::Failed {
            id,
            reason: "busy".into(),
        });
        assert_eq!(p.typed(), "land it", "back for a retry");
        p.take(p.live[1..].to_vec(), Vec::new());
        assert_eq!(p.typed(), "", "not over the next card");
        let t = text(&view(&p, &ctx(&[], &[]), 90, 36));
        assert!(
            t.contains("not sent, migrations's card ended: land it"),
            "{t}"
        );
    }

    /// The words of every card that ends stay on screen, one card's never in place of
    /// another's, until an esc outside typing and the whole view; `t`, an answer and the whole
    /// view keep them. The four orders once found losing words.
    #[test]
    fn words_that_can_no_longer_be_sent_stay_until_esc() {
        // The frame's words in one line, so words wrapped over two rows still read as typed.
        let shown = |p: &Pane| {
            text(&view(p, &ctx(&[], &[]), 90, 36))
                .split(|c: char| c == '\u{2502}' || c.is_whitespace())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let sent = |p: &mut Pane, words: &[u8]| {
            p.key(b"r", 1);
            p.key(words, 1);
            p.key(b"\r", 1);
            match p.tick(Instant::now()).0.into_iter().next() {
                Some(ControlRequest::AskWrite { id, .. }) => id,
                other => panic!("sent: {other:?}"),
            }
        };
        let failed = |p: &mut Pane, id: String| {
            p.answered(&ControlResponse::Failed {
                id,
                reason: "closed".into(),
            });
        };
        // Two cards end in two reads, each with its draft: both stand.
        let mut p = pane(3);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"draft one", 1);
        p.key(b"\x1b", 1);
        p.key(b"j", 1);
        p.key(b"r", 1);
        p.key(b"draft two", 1);
        p.key(b"\x1b", 1);
        p.take(p.live[1..].to_vec(), Vec::new());
        p.take(p.live[1..].to_vec(), Vec::new());
        let t = shown(&p);
        assert!(t.contains("draft one") && t.contains("draft two"), "{t}");
        p.key(b"\x1b", 1);
        assert!(!shown(&p).contains("draft"), "esc lets them go");
        // The failure lands in the whole view: shown there, and on the card after esc.
        let mut p = pane(2);
        p.focus = true;
        let id = sent(&mut p, b"first answer");
        p.key(b"j", 1);
        p.key(b"m", 1);
        p.take(p.live[1..].to_vec(), Vec::new());
        failed(&mut p, id);
        assert!(
            p.whole && shown(&p).contains("first answer"),
            "{}",
            shown(&p)
        );
        p.key(b"\x1b", 1);
        assert!(!p.whole, "esc goes back to the card");
        assert!(shown(&p).contains("first answer"), "{}", shown(&p));
        // Put back into the box while typing, then the card ends and esc stops the typing.
        let mut p = pane(2);
        p.focus = true;
        let id = sent(&mut p, b"first answer");
        p.key(b"r", 1);
        p.key(b"second words", 1);
        failed(&mut p, id);
        p.take(p.live[1..].to_vec(), Vec::new());
        p.key(b"\x1b", 1);
        let t = shown(&p);
        assert!(
            t.contains("first answer") && t.contains("second words"),
            "{t}"
        );
        // `t` and an answer to the next card keep them.
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"draft one", 1);
        p.key(b"\x1b", 1);
        p.take(p.live[1..].to_vec(), Vec::new());
        p.key(b"r", 1);
        assert!(shown(&p).contains("draft one"), "{}", shown(&p));
        p.key(b"go", 1);
        p.key(b"\r", 1);
        assert!(shown(&p).contains("draft one"), "{}", shown(&p));
        p.key(b"\x1b", 1);
        assert!(!shown(&p).contains("draft one"), "{}", shown(&p));
    }

    /// At the smallest pane (30x14) a longest answer (400 characters) whose card ended reads
    /// whole: the foot shows its start and the key, the whole view scrolls through every word
    /// with `j`, and a later failure stands beside it, never in its place. On its way it said
    /// "sending".
    #[test]
    fn a_longest_lost_answer_reads_whole_in_the_smallest_pane() {
        let long: String = (1..=80)
            .map(|n| format!("w{n:03}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(long.as_bytes(), 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        let t = text(&view(&p, &ctx(&[], &[]), 90, 36));
        assert!(t.contains("sending to migrations"), "{t}");
        p.take(p.live[1..].to_vec(), Vec::new());
        p.answered(&ControlResponse::Failed {
            id,
            reason: "closed".into(),
        });
        // A second failure, for the card still open.
        p.key(b"r", 1);
        p.key(b"short", 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        p.answered(&ControlResponse::Failed {
            id,
            reason: "busy".into(),
        });
        let t = text(&view(&p, &ctx(&[], &[]), 30, 14));
        assert!(t.contains("w001") && t.contains("m reads it whole"), "{t}");
        p.key(b"m", 1);
        let mut seen = String::new();
        for _ in 0..120 {
            seen.push_str(&text(&view(&p, &ctx(&[], &[]), 30, 14)));
            p.key(b"j", 1);
        }
        for n in 1..=80 {
            assert!(seen.contains(&format!("w{n:03}")), "w{n:03}:\n{seen}");
        }
        assert!(seen.contains("busy"), "the later failure beside it");
    }

    /// At the smallest pane, a longest answer lost when its card ended yields its rows to the
    /// box `t` opens on the next card: the box, its words and its caret stay on screen, so
    /// enter never sends what he cannot see.
    #[test]
    fn a_long_lost_answer_never_hides_the_box_being_typed_in() {
        let long: String = (1..=80)
            .map(|n| format!("w{n:03}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(long.as_bytes(), 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        p.take(p.live[1..].to_vec(), Vec::new());
        p.answered(&ControlResponse::Failed {
            id,
            reason: "closed".into(),
        });
        p.key(b"r", 1);
        p.key(b"zq", 1);
        let v = view(&p, &ctx(&[], &[]), 30, 14);
        let t = text(&v);
        assert!(v.cursor.is_some(), "the caret: {t}");
        assert!(t.contains("zq\u{2588}") && t.contains("not sent"), "{t}");
    }

    /// The card read whole carries what it is about (asked, doing, why) and what each answer
    /// sends, scrolled with `j`; the card's head keeps who asks whole at a narrow width.
    #[test]
    fn the_whole_view_carries_the_context_and_what_each_answer_sends() {
        let mut p = pane(1);
        p.focus = true;
        let t = text(&view(&p, &ctx(&[], &[]), 26, 17));
        assert!(t.contains("\u{25be} migrations "), "{t}");
        p.key(b"m", 1);
        let mut seen = String::new();
        for _ in 0..60 {
            seen.push_str(&text(&view(&p, &ctx(&[], &[]), 30, 17)));
            p.key(b"j", 1);
        }
        for want in [
            "You asked",
            "migrations did",
            "migration",
            "Why it needs you",
            "staging",
            "Sends:",
            "owner.",
        ] {
            assert!(seen.contains(want), "{want}:\n{seen}");
        }
    }

    /// The words the daemon never took, for the exit line when it goes: an answer held and the
    /// lost words, each with whose card.
    #[test]
    fn the_unsent_words_name_whose_card() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"draft one", 1);
        p.key(b"\x1b", 1);
        p.take(p.live[1..].to_vec(), Vec::new());
        p.key(b"r", 1);
        p.key(b"land 42 then rebase\r", 1);
        assert_eq!(
            p.unsent(),
            [
                "not sent to web-app: land 42 then rebase",
                "not sent, migrations's card ended: draft one"
            ]
        );
    }

    /// Two typed answers to one card in flight are told apart: each fails on its own and both
    /// sets of words come back.
    #[test]
    fn two_answers_in_flight_for_one_card_each_come_back() {
        let mut p = pane(1);
        p.focus = true;
        let mut ids = Vec::new();
        for words in [&b"first try"[..], b"second try"] {
            p.key(b"r", 1);
            p.key(words, 1);
            p.key(b"\r", 1);
            match p.tick(Instant::now()).0.into_iter().next() {
                Some(ControlRequest::AskWrite { id, .. }) => ids.push(id),
                other => panic!("sent: {other:?}"),
            }
        }
        assert_ne!(ids[0], ids[1]);
        for id in ids {
            p.answered(&ControlResponse::Failed {
                id,
                reason: "no".into(),
            });
        }
        assert!(
            p.typed().contains("first try") && p.typed().contains("second try"),
            "{}",
            p.typed()
        );
    }

    /// Typing leaves the question read whole: the box and its caret are on screen at the 60x24
    /// and 80x24 sizes, never typing into a box that is not drawn.
    #[test]
    fn typing_leaves_the_question_read_whole() {
        for (cols, rows) in [(22, 17), (34, 17)] {
            let mut p = long_pane();
            p.focus = true;
            p.key(b"m", 1);
            p.key(b"r", 1);
            p.key(b"go", 1);
            let v = view(&p, &ctx(&[], &[]), cols, rows);
            let t = text(&v);
            assert!(
                t.contains("go\u{2588}") && v.cursor.is_some(),
                "{cols}x{rows}:\n{t}"
            );
        }
    }

    /// A long answer in wide letters keeps its caret in the box: the tail is taken by cells.
    #[test]
    fn a_wide_answer_keeps_its_caret() {
        let mut p = pane(1);
        p.focus = true;
        p.key(b"r", 1);
        p.key("日本語の答えです".repeat(6).as_bytes(), 1);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("です\u{2588}"), "{t}");
    }

    /// Answered elsewhere while he types (the phone): the keys stay in the box, so none of his
    /// letters becomes a command, his words stay, and enter says why it sends nothing.
    #[test]
    fn a_card_answered_elsewhere_while_typing_keeps_the_words_and_says_so() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"just do", 1);
        let mut live = p.live.clone();
        live[0].status = "passed".into();
        p.take(live, Vec::new());
        p.key(b" x and a", 1);
        assert!(
            p.typing() && p.typed() == "just do x and a",
            "{}",
            p.typed()
        );
        p.key(b"\r", 1);
        assert!(p.outbox.is_empty(), "nothing sent, nothing dismissed");
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains(HANDLED_ELSEWHERE), "{t}");
    }

    /// The card his words are for leaves the list while he types (dismissed or closed
    /// elsewhere): he goes on typing, no letter of his becomes a command, enter sends nothing,
    /// and the key line says so for as long as the words stay. Esc lets them go.
    #[test]
    fn a_card_gone_elsewhere_while_typing_keeps_the_words_and_sends_nothing() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"just do", 1);
        let rest = p.live[1..].to_vec();
        p.take(rest, Vec::new());
        p.key(b" jx", 1);
        assert!(p.typing() && p.typed() == "just do jx", "{}", p.typed());
        p.key(b"\r", 1);
        assert!(p.outbox.is_empty(), "nothing sent, nothing dismissed");
        p.tick(Instant::now() + NOTICE * 2);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains(HANDLED_ELSEWHERE) && !t.contains("esc stop typing"),
            "{t}"
        );
        p.key(b"\x1b", 1);
        assert!(!p.typing() && p.typed().is_empty());
        p.key(b"r", 1);
        p.key(b"ok\r", 1);
        assert_eq!(p.outbox.len(), 1, "the card now open takes its own words");
    }

    /// The last card leaves while he types beside qass's chat: the box with his words stays,
    /// not "Nothing waits for you.", and the line under it says so first and the keys last, in
    /// the longest form that fits, never a word cut.
    #[test]
    fn the_last_card_gone_while_typing_keeps_the_box_and_names_it() {
        let mut p = pane(1);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"run it after the backup", 1);
        p.take(Vec::new(), Vec::new());
        assert!(p.busy(), "the questions stay up while he types");
        let v = view(&p, &ctx(&[], &[]), 90, 36);
        let t = text(&v);
        assert!(
            t.contains("run it after the backup") && !t.contains("Nothing waits for you."),
            "{t}"
        );
        assert!(
            t.contains(
                "handled elsewhere: migrations's card; enter sends nothing, esc stops typing"
            ),
            "{t}"
        );
        assert!(v.cursor.is_some());
        // Narrower, what enter does goes first; whose card and the esc stay, on rows of their
        // own where they must.
        let narrow = text(&view(&p, &ctx(&[], &[]), 60, 36));
        assert!(
            narrow.contains("handled elsewhere: migrations's card; esc stops typing"),
            "{narrow}"
        );
        let tight = text(&view(&p, &ctx(&[], &[]), 30, 36));
        assert!(
            tight.contains("migrations's card;") && tight.contains("stops typing"),
            "{tight}"
        );
    }

    /// An agent's name cut to its room ends in an ellipsis, as the question beside it does, in
    /// the list and on the open card alike. No cut leaves a space before its ellipsis.
    #[test]
    fn a_cut_name_ends_in_dots() {
        assert_eq!(cut_dots("release-build", 11), "release-bu\u{2026}");
        assert_eq!(cut_dots("ab cd ef", 7), "ab cd\u{2026}");
        assert_eq!(cut_dots("short", 11), "short");
        let mut p = Pane::default();
        p.take(
            vec![
                card(1, "release-build-and-notarize", "open"),
                card(2, "web-app", "open"),
            ],
            Vec::new(),
        );
        let t = text(&view(&p, &ctx(&[], &[]), 60, 36));
        assert!(t.contains("release-bui\u{2026} "), "{t}");
        p.expanded = true;
        let t = text(&view(&p, &ctx(&[], &[]), 60, 36));
        assert!(
            t.contains("\u{25be} release-build-and-notar\u{2026} "),
            "{t}"
        );
    }

    /// The daemon's draft for an agent that waits on him has no options: the card shows its
    /// question and the box for his own words, a key moves nothing, and his words go out.
    #[test]
    fn a_draft_with_no_options_takes_his_own_words() {
        let t0 = Instant::now();
        let mut draft = card(7, "dub52", "open");
        draft.card = "W7".to_string();
        draft.body.asks = "Merge PR #513 now or deploy first?".to_string();
        draft.body.options.clear();
        draft.body.pick = None;
        let mut p = Pane::default();
        p.take(vec![draft], Vec::new());
        p.focus = true;
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        for want in ["Merge PR #513 now or deploy first?", "[r] Reply"] {
            assert!(t.contains(want), "{want:?} in\n{t}");
        }
        assert!(!t.contains("[1]") && !t.contains("[a]"), "{t}");
        p.key(b"j", 1);
        p.key(b"a", 1);
        assert!(p.tick(t0).0.is_empty(), "no option to send");
        p.key(b"r", 1);
        p.key(b"merge it", 1);
        p.key(b"\r", 1);
        let (sent, _) = p.tick(t0);
        let [
            ControlRequest::AskWrite {
                action: AskOp::Answer {
                    card, option, text, ..
                },
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("his words go out")
        };
        assert_eq!(
            (card.as_str(), option, text.as_deref()),
            ("W7", &None, Some("merge it"))
        );
    }

    /// The column can be dragged down to 24 columns: no line of the card, the text box's top edge
    /// included, is wider than the column.
    #[test]
    fn the_text_box_fits_the_narrowest_column() {
        let p = pane(1);
        for cols in [24, 30, 40] {
            let v = view(&p, &ctx(&[], &[]), cols, 36);
            for row in &v.rows {
                let w = width::width(&strip(row));
                assert!(w <= cols, "{w} columns in {cols}: {:?}", strip(row));
            }
        }
        // The box's top edge keeps both corners, with the title cut to what fits.
        let t = text(&view(&p, &ctx(&[], &[]), 24, 36));
        let top = t
            .lines()
            .find(|l| l.contains("\u{256d}\u{2500} reply"))
            .expect("the box");
        assert!(top.contains('\u{256e}'), "{t}");
        // The open card's name and its wait never run together.
        // Who asks is whole even here; the wait is short beside it.
        let head = t
            .lines()
            .find(|l| l.contains("\u{25be} migrations "))
            .expect("the head");
        assert!(!head.contains("migrations6") && head.contains("6m"), "{t}");
    }

    #[test]
    fn an_empty_inbox_says_so_and_a_missing_one_says_why() {
        let mut p = Pane::default();
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("reading the questions"));
        p.take(Vec::new(), Vec::new());
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("Nothing waits for you."));
        p.unsupported = true;
        assert!(text(&view(&p, &ctx(&[], &[]), 50, 20)).contains("restart qoop for the questions"));
        assert_eq!(p.count(), None);
    }

    #[test]
    fn keys_move_the_selected_card_and_enter_opens_and_folds_it() {
        let mut p = pane(3);
        p.focus = true;
        assert_eq!(p.open_card().unwrap().card, "E1");
        p.key(b"j", 1);
        p.key(b"j", 1);
        p.key(b"j", 1);
        assert_eq!(p.open_card().unwrap().card, "E3", "stops at the last");
        p.key(b"k", 1);
        assert_eq!(p.open_card().unwrap().card, "E2");
        // The arrows move cards as j and k do.
        p.key(b"\x1b[B", 1);
        assert_eq!(p.open_card().unwrap().card, "E3");
        p.key(b"\x1b[A", 1);
        p.key(b"\x1b[A", 1);
        assert_eq!(p.open_card().unwrap().card, "E1");
        // Every option shows what it sends once the card is open: no preview to move.
        p.key(b"\r", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 40));
        for sends in ["Sends: Run 0042", "Sends: Do not run", "Sends: Skip 0042"] {
            assert!(t.contains(sends), "{sends}:\n{t}");
        }
    }

    #[test]
    fn one_key_sends_an_answer_at_once_with_the_words_he_saw() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        // A key goes out on the very next tick: no wait, and `u` means nothing.
        p.key(b"u", 1);
        assert_eq!(p.tick(t0).0.len(), 0);
        p.key(b"b", 1);
        assert!(
            p.wake_at().is_some_and(|at| at <= Instant::now()),
            "due now"
        );
        let (sent, _) = p.tick(t0);
        let [
            ControlRequest::AskWrite {
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
            },
        ] = sent.as_slice()
        else {
            panic!("an answer goes out: {sent:?}")
        };
        assert_eq!(
            (card.as_str(), option.as_deref(), text),
            ("E1", Some("b"), &None)
        );
        assert_eq!(
            seen.as_deref(),
            Some("Do not run 0042 yet; stay idle and wait for the owner.")
        );
        assert!(p.tick(t0).0.is_empty(), "sent once");
        // Leaving the inbox takes nothing back: the answer already went.
        p.key(b"a", 1);
        p.release();
        assert_eq!(p.tick(t0).0.len(), 1);
    }

    #[test]
    fn his_own_words_go_through_the_one_box() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        p.key(b"r", 1);
        assert!(p.typing());
        p.key(b"run it", 1);
        p.key(b"\x7f", 1);
        p.key(b"\x7f", 1);
        p.key(b"twice", 1);
        let v = view(&p, &ctx(&[], &[]), 50, 36);
        assert!(text(&v).contains("run twice\u{2588}"), "{}", text(&v));
        assert!(v.cursor.is_some());
        p.key(b"\r", 1);
        assert!(!p.typing());
        let (sent, _) = p.tick(t0);
        let [
            ControlRequest::AskWrite {
                action: AskOp::Answer { option, text, .. },
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("an answer goes out")
        };
        assert_eq!((option, text.as_deref()), (&None, Some("run twice")));
        // An empty box sends nothing, and esc stops typing.
        p.key(b"r", 1);
        p.key(b"\r", 1);
        assert!(p.tick(t0).0.is_empty());
        p.key(b"\x1b", 1);
        assert!(!p.typing());
    }

    #[test]
    fn the_prefix_a_mouse_report_and_the_exit_keys_are_not_typed() {
        let mut p = pane(1);
        p.focus = true;
        assert_eq!(p.key(b"\x01", 1), Out::Pass(b"\x01".to_vec()));
        assert!(matches!(p.key(b"\x1b[<0;5;5M", 1), Out::Pass(_)));
        p.key(b"r", 1);
        assert_eq!(
            p.key(b"\x01", 1),
            Out::Pass(b"\x01".to_vec()),
            "even while typing"
        );
        p.key(b"\x1b", 1);
        assert_eq!(p.key(b"\t", 1), Out::ToChat);
        assert_eq!(p.key(b"\x1b", 1), Out::Closed);
        assert_eq!(p.key(b"g", 1), Out::Jump(1));
        // Letters that mean nothing are swallowed, never typed into the chat behind.
        assert_eq!(p.key(b"z", 1), Out::Stay);
    }

    /// `o` asks to talk the open card through; with no card it does nothing.
    #[test]
    fn o_talks_the_open_card_through() {
        let talk = card(1, "korea", "open");
        let mut p = Pane {
            focus: true,
            ..Default::default()
        };
        p.take(vec![talk.clone()], Vec::new());
        assert_eq!(p.key(b"o", 1), Out::Talk(Box::new(talk)));
        p.take(Vec::new(), Vec::new());
        assert_eq!(p.key(b"o", 1), Out::Stay);
        assert!(!p.busy(), "nothing was answered");
    }

    fn row_at(terminal: u32, workspace: i64, name: Option<&str>, cwd: Option<&str>) -> AgentRow {
        AgentRow {
            workspace,
            name: name.map(str::to_string),
            cwd: cwd.map(str::to_string),
            ..agent_row(terminal)
        }
    }

    fn space_row(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        }
    }

    /// Talk it through goes to the agent the card is about, in the phone's order: its terminal,
    /// its `T5` or `devbox/T5` agent, such a reference in its words, the agent its words name.
    #[test]
    fn talk_it_through_goes_to_the_agent_the_card_is_about() {
        let mut c = card(1, "websearch", "open");
        c.agent = None;
        c.terminal = None;
        let qass_space = 9;
        let spaces = vec![
            space_row(qass_space, sidebar::QASS_SPACE),
            space_row((1 << 40) | 2, "devbox:main"),
        ];
        let remote = crate::mux::REMOTE_TERMINAL | (1 << 24) | 25;
        let mut rows = vec![
            row_at(3, qass_space, Some("qass"), None),
            row_at(8, 1, None, Some("/Users/pat/coding/blog")),
            row_at(639, 1, None, Some("/Users/pat/coding/shop")),
            row_at(644, 1, None, Some("/Users/pat/coding/shop")),
            row_at(remote, (1 << 40) | 2, None, None),
        ];
        rows[2].last_active_at_unix = Some(100);
        rows[3].last_active_at_unix = Some(200);
        // About nothing: qass.
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // Its terminal, whatever the card's number says (E600 and T639 live).
        c.terminal = Some(639);
        assert_eq!(talk_target(&c, &rows, &spaces), Some(639));
        c.terminal = None;
        // A remote agent, told by its remote's name and its number there (E581 live).
        c.agent = Some("devbox/T25".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), Some(remote));
        c.agent = Some("build1/T25".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // A terminal that is gone is no tab: the card's agent, else qass.
        c.terminal = Some(77);
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        c.card = "W1".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(77));
        c.card = "E1".to_string();
        c.terminal = None;
        // qass's own tab is no agent a card names.
        c.agent = Some("T3".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // A reference in its words.
        c.agent = None;
        c.body.doing = "T8 is waiting on the build".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        c.body.doing = "T8-worker is stuck".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        c.body.doing = "wrote migration 0042".to_string();
        // E601: nothing but "Shop" in its words and two agents in the shop folder, the last active wins.
        c.body.handle = "shoppay".to_string();
        c.body.asks = "Stripe account and mail sender for Shop?".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(644));
        // The card's task is an agent's task.
        c.body.asks = "Which one?".to_string();
        c.body.handle = "x".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        c.task = Some(510);
        rows[1].task = Some(qoop_runtime::control::TaskRef {
            id: 510,
            goal: "g".to_string(),
            stage: "implementing".to_string(),
            claimed: false,
        });
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        // qass is never the agent a card names, and a branch-like name matches on no everyday word.
        c.task = None;
        c.body.asks = "Ask qass to wake it, in his words?".to_string();
        rows[1].name = Some("fix/draft-card-owner-words".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
    }

    #[test]
    fn a_passed_card_cannot_be_answered_again_and_says_where_it_is() {
        let t0 = Instant::now();
        let mut p = Pane::default();
        let mut c = card(1, "cv", "passed");
        c.closed_at = Some(940);
        p.take(vec![c], Vec::new());
        p.focus = true;
        p.key(b"a", 1);
        p.key(b"r", 1);
        assert!(p.tick(t0).0.is_empty() && !p.typing());
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("passed to qass, 1 min ago") && !t.contains("[1]"),
            "no answers on a card that is not his: {t}"
        );
        p.key(b"\r", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("waiting for qass"), "{t}");
        assert_eq!(p.waiting(), 0, "a passed card is not waiting for him");
    }

    #[test]
    fn the_daemon_answers_become_one_calm_line_saying_what_went_and_to_whom() {
        let t0 = Instant::now();
        let mut p = pane(1);
        p.focus = true;
        let wrote = |p: &mut Pane| {
            p.key(b"a", 1);
            let (sent, _) = p.tick(t0);
            let Some(ControlRequest::AskWrite { id, .. }) = sent.into_iter().next() else {
                panic!("sent")
            };
            id
        };
        let said = |p: &Pane| text(&view(p, &ctx(&[], &[]), 100, 36));
        let id = wrote(&mut p);
        assert!(p.answered(&ControlResponse::AskWritten {
            id,
            card: "E1".into(),
            result: AskResult::Answered,
        }));
        assert!(p.stale, "the inbox is read again");
        assert!(
            said(&p).contains("sent to migrations: Run 0042 on staging now"),
            "{}",
            said(&p)
        );
        // A card that goes through qass says so, queued or not.
        let id = wrote(&mut p);
        p.answered(&ControlResponse::AskWritten {
            id,
            card: "E1".into(),
            result: AskResult::Passed,
        });
        assert!(said(&p).contains("sent to qass: Run 0042 on staging now"));
        // A refusal is shown as the daemon worded it.
        let id = wrote(&mut p);
        p.answered(&ControlResponse::Failed {
            id,
            reason: "E1 changed since you read it; look again".into(),
        });
        assert!(said(&p).contains("changed since you read it"));
        assert!(p.sent.is_empty());
        // A daemon from before answers were queued may still say busy.
        let id = wrote(&mut p);
        p.answered(&ControlResponse::AskWritten {
            id,
            card: "E1".into(),
            result: AskResult::Busy,
        });
        assert!(said(&p).contains("qass is busy, answer migrations in the chat"));
        // An older daemon: the pane says so.
        let mut old = Pane::default();
        assert!(old.answered(&ControlResponse::Failed {
            id: String::new(),
            reason: "unknown variant `AskRead`, expected one of `X`".into(),
        }));
        assert!(old.unsupported);
    }

    #[test]
    fn several_answers_in_flight_each_report_their_own_words() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"a", 1);
        p.key(b"j", 1);
        p.key(b"c", 1);
        let (sent, _) = p.tick(t0);
        assert_eq!(sent.len(), 2);
        let ids: Vec<String> = sent
            .into_iter()
            .map(|r| match r {
                ControlRequest::AskWrite { id, .. } => id,
                other => panic!("{other:?}"),
            })
            .collect();
        for id in ids.iter().rev() {
            p.answered(&ControlResponse::AskWritten {
                id: id.clone(),
                card: "E1".into(),
                result: AskResult::Answered,
            });
        }
        assert!(p.sent.is_empty(), "both were answered");
    }

    #[test]
    fn a_fresh_read_keeps_the_open_card_when_it_is_still_there() {
        let mut p = pane(3);
        p.key(b"j", 1);
        let kept: Vec<AskInfo> = p.live.iter().filter(|c| c.card != "E1").cloned().collect();
        p.take(kept.clone(), Vec::new());
        assert_eq!(p.open_card().unwrap().card, "E2");
    }

    #[test]
    fn old_cards_dim_and_the_rows_tag_what_changed() {
        let mut p = pane(1);
        p.live[0].opened_at = 1_000 - 26 * 3600;
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("waiting 26 h"), "{t}");
        p.expanded = false;
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("migrations") && t.contains("26h"), "{t}");
        // The old card's folded row is painted in the calm ink, a fresh one's in the focused ink.
        let row_of = |p: &Pane| {
            view(p, &ctx(&[], &[]), 50, 36)
                .rows
                .into_iter()
                .find(|r| strip(r).contains("migrations"))
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
            hit_at(&v, rect, 7 + row_of("web-app"), 10),
            Some(Hit::Card(1))
        );
        assert_eq!(
            hit_at(&v, rect, 7 + row_of("[2] wait"), 10),
            Some(Hit::Option(1))
        );
        assert_eq!(
            hit_at(&v, rect, 7 + row_of("r to reply"), 10),
            Some(Hit::Box)
        );
        let tabs = row_of("questions");
        assert_eq!(hit_at(&v, rect, 7 + tabs, 2 + 16), Some(Hit::Board));
        assert_eq!(hit_at(&v, rect, 7 + tabs, 3), None);
        assert_eq!(hit_at(&v, rect, 6, 10), None, "above the box");
        let mut p = pane(3);
        p.click(Hit::Card(2));
        assert_eq!(p.open_card().unwrap().card, "E3");
        assert!(p.is_open(), "a click on a card opens it");
        p.click(Hit::Card(2));
        assert!(!p.is_open(), "and a click on the open one folds it");
        p.click(Hit::Option(0));
        assert_eq!(p.tick(Instant::now()).0.len(), 1);
        let mut q = pane(1);
        q.click(Hit::Box);
        assert!(q.typing());
    }

    /// On a folded card each answer and `[r] Reply` is a click of its own, on any card: it
    /// selects that card first.
    #[test]
    fn a_click_on_a_folded_answer_sends_it_from_that_card() {
        let mut p = Pane::default();
        p.take(
            vec![card(1, "migrations", "open"), card(2, "web-app", "open")],
            Vec::new(),
        );
        let v = view(&p, &ctx(&[], &[]), 90, 20);
        let rect = Rect {
            x: 0,
            y: 0,
            cols: 90,
            rows: 20,
        };
        let rows: Vec<usize> = v
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| strip(r).contains("[1] run now"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(rows.len(), 2, "{}", text(&v));
        // `[1] run now` starts at column 4 of the card's rows: 2 for the box and 4 indent.
        assert_eq!(hit_at(&v, rect, rows[1], 7), Some(Hit::Answer(1, 0)));
        let row = strip(&v.rows[rows[1]]);
        let reply = row.find("[r] Reply").unwrap();
        let col = row[..reply].chars().count() + 2;
        assert_eq!(hit_at(&v, rect, rows[1], col), Some(Hit::Reply(1)));
        assert_eq!(hit_at(&v, rect, rows[1] - 1, 10), Some(Hit::Card(1)));
        p.click(Hit::Answer(1, 2));
        assert_eq!(p.open_card().unwrap().card, "E2");
        assert_eq!(p.outbox.len(), 1);
        p.outbox.clear();
        p.click(Hit::Reply(0));
        assert!(p.typing() && p.open_card().unwrap().card == "E1");
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
            short.contains("[1] run now") && short.contains("reply in your own words"),
            "{short}"
        );
    }

    /// A sent answer's word stands a few seconds; a failure stays until esc.
    #[test]
    fn a_notice_stands_a_few_seconds_and_a_failure_until_esc() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        p.key(b"a", 1);
        let (sent, _) = p.tick(t0);
        let Some(ControlRequest::AskWrite { id, .. }) = sent.into_iter().next() else {
            panic!("sent")
        };
        p.answered(&ControlResponse::AskWritten {
            id,
            result: AskResult::Answered,
            card: String::new(),
        });
        assert!(p.wake_at().is_some(), "a clock to let it go");
        assert!(!p.tick(t0 + Duration::from_secs(1)).1);
        assert!(
            p.tick(t0 + NOTICE + Duration::from_secs(1)).1,
            "repainted once to let go"
        );
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(
            t.contains("1-9 answer") && t.contains("r reply") && !t.contains("sent to"),
            "{t}"
        );
        assert_eq!(p.wake_at(), None, "nothing armed again");
        p.key(b"b", 1);
        let (sent, _) = p.tick(t0);
        let Some(ControlRequest::AskWrite { id, .. }) = sent.into_iter().next() else {
            panic!("sent")
        };
        p.answered(&ControlResponse::Failed {
            id,
            reason: "taken back".into(),
        });
        assert!(!p.tick(t0 + NOTICE * 10).1, "no clock lets a failure go");
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("taken back") && !t.contains("j k move"), "{t}");
        p.key(b"\x1b[B", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("taken back"), "a key in the pane keeps it: {t}");
        p.key(b"\x1b", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 50, 36));
        assert!(t.contains("j k move") && !t.contains("taken back"), "{t}");
    }

    /// An option or a dismissal takes the card's draft with it, and a failure brings it back.
    #[test]
    fn a_draft_comes_back_when_an_option_or_a_dismissal_fails() {
        for key in [b"a", b"x"] {
            let mut p = pane(2);
            p.focus = true;
            p.key(b"r", 1);
            p.key(b"only after the backup", 1);
            p.key(b"\x1b", 1);
            p.key(key, 1);
            assert_eq!(p.typed(), "", "{key:?} takes the draft");
            let Some(ControlRequest::AskWrite { id, .. }) =
                p.tick(Instant::now()).0.into_iter().next()
            else {
                panic!("sent")
            };
            p.answered(&ControlResponse::Failed {
                id,
                reason: "no".into(),
            });
            assert_eq!(p.typed(), "only after the backup", "{key:?}");
        }
    }

    /// The words of a card that ended stand whole, wrapped in as many rows as they need, until
    /// esc: a key in the pane does not take them away.
    #[test]
    fn the_words_of_a_card_that_ended_stand_whole_until_esc() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(words(20).as_bytes(), 1);
        p.key(b"\r", 1);
        let Some(ControlRequest::AskWrite { id, .. }) = p.tick(Instant::now()).0.into_iter().next()
        else {
            panic!("sent")
        };
        p.take(p.live[1..].to_vec(), Vec::new());
        p.answered(&ControlResponse::Failed {
            id,
            reason: "closed".into(),
        });
        p.key(b"j", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 30, 36));
        for word in words(20).split(' ') {
            assert!(t.contains(word), "{word}:\n{t}");
        }
        p.key(b"\x1b", 1);
        let t = text(&view(&p, &ctx(&[], &[]), 30, 36));
        assert!(!t.contains("word20"), "{t}");
    }

    /// A typed answer whose write fails comes back in the box with a way to send it again; one
    /// the daemon took is gone.
    #[test]
    fn a_typed_answer_comes_back_when_its_write_fails() {
        let t0 = Instant::now();
        let mut p = pane(2);
        p.focus = true;
        let send = |p: &mut Pane, words: &[u8]| {
            p.key(b"r", 1);
            p.key(words, 1);
            p.key(b"\r", 1);
            assert_eq!(p.typed(), "", "the box empties as it goes");
            match p.tick(t0).0.into_iter().next() {
                Some(ControlRequest::AskWrite { id, .. }) => id,
                other => panic!("sent: {other:?}"),
            }
        };
        let id = send(&mut p, b"land 42 first");
        p.answered(&ControlResponse::Failed {
            id,
            reason: "the daemon is restarting".into(),
        });
        assert_eq!(p.typed(), "land 42 first");
        let t = text(&view(&p, &ctx(&[], &[]), 70, 36));
        assert!(
            t.contains("not sent, t and enter retry: the daemon is restarting"),
            "{t}"
        );
        // Sent again and taken: nothing comes back.
        p.key(b"r", 1);
        p.key(b"\r", 1);
        let id = match p.tick(t0).0.into_iter().next() {
            Some(ControlRequest::AskWrite { id, .. }) => id,
            other => panic!("sent: {other:?}"),
        };
        p.answered(&ControlResponse::AskWritten {
            id,
            result: AskResult::Answered,
            card: String::new(),
        });
        assert_eq!(p.typed(), "");
        // Failed after he moved to another card: kept for its card.
        let id = send(&mut p, b"wait for me");
        p.key(b"j", 1);
        p.answered(&ControlResponse::Failed {
            id,
            reason: "no".into(),
        });
        assert_eq!(p.typed(), "");
        p.key(b"k", 1);
        assert_eq!(p.typed(), "wait for me");
        // Failed while new words are in the box: both are kept, the failed ones first.
        p.key(b"r", 1);
        p.key(b"\r", 1);
        let id = match p.tick(t0).0.into_iter().next() {
            Some(ControlRequest::AskWrite { id, .. }) => id,
            other => panic!("sent: {other:?}"),
        };
        p.key(b"r", 1);
        p.key(b"and then", 1);
        p.answered(&ControlResponse::Failed {
            id,
            reason: "no".into(),
        });
        assert_eq!(p.typed(), "wait for me and then");
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
    fn words_typed_for_a_card_are_not_kept_for_the_one_that_replaced_it() {
        let mut p = pane(2);
        p.focus = true;
        p.key(b"r", 1);
        p.key(b"for the first", 1);
        // The card he types for is gone: his words stay while he types and go with esc, never
        // to the next one.
        p.take(p.live[1..].to_vec(), Vec::new());
        assert_eq!(p.typed(), "for the first");
        p.key(b"\x1b", 1);
        assert!(!p.typing() && p.typed().is_empty());
        // Typed text changes with a move to another card, and escapes are not letters.
        p.key(b"r", 1);
        p.key(b"\x1b[D", 1);
        p.key(b"\x1b[200~", 1);
        assert_eq!(p.typed(), "");
        p.key(b"ok", 1);
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

    fn words(n: usize) -> String {
        (1..=n)
            .map(|i| format!("word{i:02}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn long_pane() -> Pane {
        let mut c = card(1, "migrations", "open");
        c.body.asks = words(24);
        c.body.doing = words(12);
        c.body.options[0].reason = words(10);
        c.body.options[1].label = "a label far longer than a third of the column".into();
        c.body.options[1].reason = format!("{} {}", "x".repeat(70), "\u{4e2d}".repeat(30));
        let mut p = Pane::default();
        p.take(vec![c], Vec::new());
        p.expanded = true;
        p
    }

    #[test]
    fn wrap_dots_wraps_on_words_and_ends_in_dots_only_when_text_is_left() {
        assert_eq!(wrap_dots("ab cd ef", 5, 3), ["ab cd", "ef"]);
        assert_eq!(wrap_dots("ab cd", 5, 3), ["ab cd"]);
        assert_eq!(wrap_dots("abcdefgh ij", 4, 5), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_dots("ab cd ef gh ij", 5, 2), ["ab cd", "ef g\u{2026}"]);
        assert_eq!(wrap_dots("ab cd ef gh", 5, 1), [cut_dots("ab cd ef gh", 5)]);
        assert_eq!(
            wrap_dots("\u{4e2d}\u{4e2d}\u{4e2d}", 4, 3),
            ["\u{4e2d}\u{4e2d}", "\u{4e2d}"]
        );
        assert_eq!(
            wrap_dots("\u{4e2d}x", 1, 3),
            ["x"],
            "a wide char that never fits is dropped"
        );
        assert_eq!(wrap_dots("", 5, 3), [""]);
    }

    #[test]
    fn a_long_question_and_its_reasons_are_read_whole_when_the_pane_has_the_rows() {
        let p = long_pane();
        let t = text(&view(&p, &ctx(&[], &[]), 56, 44));
        for n in 1..=24 {
            assert!(t.contains(&format!("word{n:02}")), "question word {n}\n{t}");
        }
        assert!(!t.contains("word24\u{2026}"), "{t}");
        for n in 1..=10 {
            assert!(t.contains(&format!("word{n:02}")), "reason word {n}");
        }
    }

    /// A short question with an answer cut to its room (the 60x24 window's 22 columns) names `m`
    /// on the key line, and `m` reads that answer whole.
    #[test]
    fn an_answer_cut_to_its_room_is_read_whole_with_m() {
        let mut p = long_pane();
        let mut card = p.live[0].clone();
        card.body.asks = "Which first?".into();
        card.body.options[0].reason = "smaller, already reviewed by two people".into();
        p.take(vec![card], Vec::new());
        p.focus = true;
        let t = text(&view(&p, &ctx(&[], &[]), 22, 17));
        assert!(t.contains("m read whole"), "{t}");
        p.key(b"m", 1);
        let mut seen = String::new();
        for _ in 0..40 {
            seen.push_str(&text(&view(&p, &ctx(&[], &[]), 22, 17)));
            p.key(b"j", 1);
        }
        assert!(seen.contains("two") && seen.contains("people"), "{seen}");
    }

    /// A short pane keeps every answer, the key line and a box with words in it: the question
    /// takes the rows left, cut with `m` named to read it whole. While it is read whole the
    /// answers are not on screen and `a` to `d` send nothing.
    #[test]
    fn a_short_pane_keeps_every_answer_and_m_reads_the_question_whole() {
        let mut p = long_pane();
        let mut card = p.live[0].clone();
        card.body.asks = words(30);
        let mut d = card.body.options[0].clone();
        d.key = "d".into();
        card.body.options.push(d);
        p.take(vec![card], Vec::new());
        p.focus = true;
        for (cols, rows) in [(34, 17), (22, 17), (34, 14)] {
            for draft in ["", "keep this"] {
                p.text = draft.into();
                let t = text(&view(&p, &ctx(&[], &[]), cols, rows));
                for key in ["[1]", "[2]", "[3]", "[4]"] {
                    assert!(t.contains(key), "{key} at {cols}x{rows} {draft:?}:\n{t}");
                }
                assert!(
                    t.contains("word01") && t.contains("m read whole"),
                    "{cols}x{rows}:\n{t}"
                );
                assert!(t.contains(draft), "the box keeps its words:\n{t}");
                // Cut, the question starts right under the card's title: no blank row is kept.
                let lines: Vec<&str> = t.lines().collect();
                let head = lines.iter().position(|l| l.contains("\u{25be} ")).unwrap();
                assert!(lines[head + 1].contains("word01"), "{cols}x{rows}:\n{t}");
            }
        }
        p.text.clear();
        // Read whole: every row but the key line, `j k` scroll it, at 34x17 and at 22x17 (the
        // 60x24 target) with a 210 character question.
        for (cols, rows) in [(34, 17), (22, 17)] {
            p.key(b"m", 1);
            let mut seen = String::new();
            for _ in 0..80 {
                let t = text(&view(&p, &ctx(&[], &[]), cols, rows));
                assert!(!t.contains("questions"), "{t}");
                seen.push_str(&t);
                p.key(b"j", 1);
            }
            // The question and every answer with its reason, wrapped, none cut.
            let reasons = words(10);
            for word in words(30).split(' ').chain(["[1]", "[2]", "[3]", "[4]"]) {
                assert!(seen.contains(word), "{word} read whole at {cols}x{rows}");
            }
            assert!(
                seen.contains(reasons.split(' ').next_back().unwrap()),
                "{seen}"
            );
            if cols == 22 {
                assert!(seen.contains("j k scroll  m back"), "{seen}");
            }
            assert_eq!(p.selected, 0, "j scrolls, it does not change the card");
            p.key(b"m", 1);
        }
        // Read whole in a short pane the answers are below the fold: `a` sends nothing; esc
        // goes back to the card, where `a` sends.
        p.key(b"m", 1);
        let _ = view(&p, &ctx(&[], &[]), 22, 10);
        p.key(b"a", 1);
        assert!(p.outbox.is_empty(), "no answer sent that is not on screen");
        assert!(
            matches!(p.key(b"\x1b", 1), Out::Stay),
            "esc back to the card"
        );
        let _ = view(&p, &ctx(&[], &[]), 34, 17);
        p.key(b"a", 1);
        assert_eq!(p.outbox.len(), 1, "back on screen, `a` sends");
    }

    #[test]
    fn long_cards_fit_every_size_and_keep_the_caret_off_the_key_line() {
        for typing in [false, true] {
            let mut p = long_pane();
            p.focus = true;
            p.typing = typing;
            p.text = words(30);
            for cols in 25..=80 {
                for rows in 11..=60 {
                    let v = view(&p, &ctx(&[], &[]), cols, rows);
                    assert_eq!(v.rows.len(), rows, "{cols}x{rows}");
                    for (i, r) in v.rows.iter().enumerate() {
                        assert_eq!(width::width(&strip(r)), cols, "{cols}x{rows} row {i}");
                    }
                    assert!(v.targets.iter().all(|t| t.row < rows - 2 && t.x1 <= cols));
                    if let Some((r, _)) = v.cursor {
                        assert!(r <= rows - 3, "caret on row {r} of {rows} at {cols}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_click_on_a_later_row_of_a_wrapped_reason_hits_its_option() {
        let p = long_pane();
        let v = view(&p, &ctx(&[], &[]), 56, 44);
        let rect = Rect {
            x: 0,
            y: 0,
            cols: 56,
            rows: 44,
        };
        let first = v
            .rows
            .iter()
            .position(|r| strip(r).contains("[1] "))
            .unwrap();
        assert_eq!(hit_at(&v, rect, first, 10), Some(Hit::Option(0)));
        assert_eq!(hit_at(&v, rect, first + 1, 10), Some(Hit::Option(0)));
    }

    #[test]
    fn a_card_with_long_values_shows_all_its_text_and_gives_rows_back_one_at_a_time() {
        let mut c = card(1, "web", "open");
        c.body.asked =
            Some("drop users.legacy_id and move the audit trail to the new events table before the freeze".into());
        c.body.doing = "wrote migration 0042, dry run on the copy is clean, and the rollback script is ready but not yet tested on staging data".into();
        c.body.why = "the PR cannot open before 0042 has run once on staging, and the freeze window starts on Friday morning for everyone".into();
        c.body.options[0].reason = "no rollback exists and staging is a copy of last week, so a bad run costs a day of restoring it".into();
        c.body.options[1].reason = "you may want to read the SQL first, since it also rewrites the audit trail rows in place".into();
        let mut p = Pane::default();
        p.take(vec![c], Vec::new());
        let t = text(&view(&p, &ctx(&[], &[]), 60, 50));
        assert!(!t.contains('\u{2026}'), "{t}");
        // Shorter and shorter: the rows given back are fewer than all at once, the text
        // loses its tail on a row at a time and the pane keeps its exact size.
        let mut cut = Vec::new();
        for rows in (14..=50).rev() {
            let v = view(&p, &ctx(&[], &[]), 60, rows);
            assert_eq!(v.rows.len(), rows);
            cut.push(text(&v).matches('\u{2026}').count());
        }
        assert!(cut.windows(2).all(|w| w[0] <= w[1] + 3), "{cut:?}");
    }
}
