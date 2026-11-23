//! The Inbox pane: everything that needs the person, one card each, oldest first, over the
//! pane. `ctrl-a i` opens it from any tab.
//!
//! What is listed is [`crate::inbox::Inbox`], built from the rows and cards this client holds;
//! what is answered goes through [`crate::answers::Holds`]: a digit starts a three second hold
//! with Undo and Send now, then the daemon is asked (`AnswerMenu`, `AnswerMenuText`,
//! `HandToQass` for an agent's menu, `AskWrite` for a card; a reply is typed into the agent's
//! terminal). Plain values and functions: no screen and no clock of its own, so a test reads
//! the rows it painted instead of looking at one.
//!
//! Text is never cut: a card's words wrap whole, and the options wrap at option boundaries so a
//! number never leaves its label.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use qoop_runtime::control::{
    AgentRow, AskInfo, AskOp, ControlRequest, ControlResponse, Hand, WorkspaceRow,
};

use crate::answers::{
    HOLD, HandInput, Hold, Holds, Offer, Pick, Slot, offers, refusal_words, text_key,
};
use crate::asks::wrap_whole;
use crate::inbox::{Context, Inbox, Item, ItemKind, OwnAnswer};
use crate::mux::machine_of;
use crate::qass_screen::{Seg, bold, line, seg};
use crate::theme::Role;
use crate::width;

/// The prefix of every request id this pane asks under, so the loop can tell its answers.
pub const ID: &str = "inbox-";

/// The columns below which opening the Inbox folds the agents column to its strip: the cards
/// need the room, and a card's words are never cut.
pub const INBOX_FOLDS_BELOW: u16 = 100;

/// How long a word about what just happened stands on the notice line.
const NOTICE: Duration = Duration::from_secs(6);

/// How long a busy agent is left before its answer is asked once more.
const RETRY: Duration = Duration::from_millis(300);

/// The most characters of the person's own words one answer carries (the daemon's limit for a
/// card; a menu's is larger).
const TEXT_MAX: usize = 400;

/// The own answers kept for Answered today.
const OWN_MOST: usize = 50;

/// The columns of a card's own words, in from the pane's edge, and of its title.
const BODY: usize = 5;
const TITLE: usize = 3;

/// What this pane reads and asks, each call: the client's rows and cards, what it knows about
/// qass, and its clocks.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    pub rows: &'a [AgentRow],
    pub spaces: &'a [WorkspaceRow],
    /// Every card read: open, answered and ended alike.
    pub cards: &'a [AskInfo],
    pub hand: &'a HandInput,
    pub ctx: &'a Context,
    pub now: Instant,
    pub now_unix: i64,
    /// When the person last typed into each terminal: a reply is never typed into a line they
    /// began after it was held.
    pub typed: &'a BTreeMap<u32, Instant>,
}

impl Env<'_> {
    fn inbox(&self, own: &[OwnAnswer]) -> Inbox {
        Inbox::build(self.rows, self.cards, self.hand, self.ctx, own)
    }

    fn row(&self, terminal: u32) -> Option<&AgentRow> {
        self.rows.iter().find(|r| r.terminal == terminal)
    }

    fn card(&self, id: &str) -> Option<&AskInfo> {
        self.cards.iter().find(|c| c.card == id)
    }

    /// The space an agent is in, as a person names it.
    fn space_name(&self, workspace: i64) -> String {
        self.spaces
            .iter()
            .find(|s| s.id == workspace)
            .map(|s| s.name.clone())
            .unwrap_or_default()
    }

    /// The computer a space is on as a person names it: "This Mac", or the machine before
    /// the colon of a remote space's name.
    fn machine_name(&self, workspace: i64) -> String {
        if machine_of(workspace) == 0 {
            return "This Mac".into();
        }
        let name = self.space_name(workspace);
        name.split_once(':')
            .map_or(name.clone(), |(machine, _)| machine.to_string())
    }

    /// Whom an item is about, as the card names it: "space / agent", else the card's handle.
    pub fn who(&self, item: &Item) -> String {
        let terminal = match &item.slot {
            Slot::Agent(t) => Some(*t),
            Slot::Card(id) => item
                .terminal
                .or_else(|| self.card(id).and_then(|c| c.terminal)),
        };
        self.who_of(terminal, &item.slot)
    }

    /// [`Env::who`] for a slot alone, as a hold names its target.
    pub fn who_slot(&self, slot: &Slot) -> String {
        let terminal = match slot {
            Slot::Agent(t) => Some(*t),
            Slot::Card(id) => self.card(id).and_then(|c| c.terminal),
        };
        self.who_of(terminal, slot)
    }

    fn who_of(&self, terminal: Option<u32>, slot: &Slot) -> String {
        let row = terminal.and_then(|t| self.row(t));
        match (row, slot) {
            (Some(row), _) => format!(
                "{} / {}",
                self.space_name(row.workspace),
                crate::sidebar::label(row)
            ),
            (None, Slot::Card(id)) => self
                .card(id)
                .map_or_else(|| "qass".to_string(), |c| c.body.handle.clone()),
            (None, Slot::Agent(t)) => format!("tab {t}"),
        }
    }
}

/// What a key or a click asks of the loop that owns the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Taken: repaint.
    Stay,
    /// The Inbox is put away.
    Closed,
    /// Go to this agent's tab.
    Jump(u32),
    /// Talk this card through: with the agent it is about, else in qass's chat.
    Talk(Box<AskInfo>),
    /// Not the Inbox's: the prefix, a mouse report or a detach, for the ordinary path.
    Pass(Vec<u8>),
}

/// Bytes to put into an agent's terminal: a reply, or the key of an answer an older machine
/// would not take as a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    pub terminal: u32,
    pub bytes: Vec<u8>,
    /// Pasted as a person pastes, rather than typed.
    pub paste: bool,
}

/// What the pane has to send now: requests for the daemon and bytes for terminals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sends {
    pub requests: Vec<ControlRequest>,
    pub typed: Vec<Typed>,
}

impl Sends {
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty() && self.typed.is_empty()
    }
}

/// What a click landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Somewhere on an item: selects it.
    Item(Slot),
    /// One of an item's answers, by its place in [`Pane::opts`].
    Opt(Slot, usize),
    /// Hand to qass.
    Hand(Slot),
    /// Reply in one's own words.
    Reply(Slot),
    /// Undo the held answer of an item.
    Undo(Slot),
    /// Send the held answer of an item now.
    SendNow(Slot),
    /// The From qass filter.
    Filter,
    /// A quiet row (with qass, finished): go to its agent.
    Go(u32),
}

/// A cell range on one row of the pane that a click answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub row: usize,
    pub x0: usize,
    pub x1: usize,
    pub hit: Hit,
}

/// The Inbox painted: one string per row, each exactly as wide as asked, the same rows as
/// plain text, and where a click lands.
#[derive(Clone, Debug)]
pub struct View {
    pub rows: Vec<String>,
    pub plain: Vec<String>,
    pub targets: Vec<Target>,
    /// Where typing goes, while the box has the keys.
    pub cursor: Option<(usize, usize)>,
}

/// What one answer is, as the card offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt {
    /// The digit that answers it, none for a standing permission (click only).
    pub digit: Option<u8>,
    pub label: String,
    pub act: Act,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Held for three seconds, then sent.
    Pick(Pick),
    /// Open the agent: nothing is sent.
    Go,
}

/// The Inbox as this client keeps it.
#[derive(Debug, Default)]
pub struct Pane {
    /// Shown over the pane, with the keyboard.
    pub open: bool,
    /// The left column was folded to its strip by opening the Inbox (below 100 columns).
    pub folded: bool,
    /// Only what qass wrote (a card of its own, a plan to approve).
    pub filter: bool,
    sel: Option<Slot>,
    /// The selected card read whole, in place of the list.
    whole: bool,
    /// Where the person is typing their own words, and what they have typed.
    typing: Option<Slot>,
    text: String,
    drafts: BTreeMap<Slot, String>,
    pub holds: Holds,
    /// What is said under a card: why an answer was refused, or that a reply was kept.
    said: BTreeMap<Slot, String>,
    /// The words of a card option as the person saw them, kept while it is held.
    seen_words: BTreeMap<Slot, String>,
    out: Sends,
    writes: u64,
    /// Answers asked and not yet answered, by request id, with whether they were asked again.
    sending: Vec<(String, Hold, bool)>,
    retry: Vec<(Instant, Hold)>,
    /// The answers this client sent, for Answered today.
    pub own: Vec<OwnAnswer>,
    /// Machines that refused a hand for This Mac's qass as too old: read back into the hand
    /// input.
    pub no_hands: BTreeSet<usize>,
    notice: Option<(String, Instant)>,
    /// The card a "sent to ..." notice is about, so a new turn of that agent's menu can take it
    /// down: it must not stand under the same question asked again.
    notice_slot: Option<Slot>,
    /// The last text seal and turn seal each agent's menu was seen with.
    turns: BTreeMap<u32, (u64, Option<u64>)>,
    /// The first body row shown, and the most it can be; whether the view follows the
    /// selection (a key moved it) or the wheel has taken it.
    offset: Cell<usize>,
    max_offset: Cell<usize>,
    follow: Cell<bool>,
}

impl Pane {
    pub fn new() -> Pane {
        Pane::default()
    }

    /// Open the Inbox over the pane.
    pub fn open(&mut self) {
        self.open = true;
        self.whole = false;
        self.follow.set(true);
    }

    /// Put the Inbox away. What is held stays held and is sent when its time is up.
    pub fn close(&mut self) {
        self.open = false;
        self.whole = false;
        self.stash_typing();
        self.said.clear();
    }

    /// Stop typing: the words stay as a draft for their card, to come back with `r`.
    fn stash_typing(&mut self) {
        if let Some(slot) = self.typing.take() {
            let words = std::mem::take(&mut self.text);
            if !words.is_empty() {
                self.drafts.insert(slot, words);
            }
        }
    }

    /// The Inbox, as this pane lists it.
    pub fn inbox(&self, env: &Env) -> Inbox {
        env.inbox(&self.own)
    }

    /// The slots a key walks: what needs the person (filtered), then what waits with qass,
    /// then what finished.
    fn order(&self, inbox: &Inbox) -> Vec<Slot> {
        let mut order: Vec<Slot> = inbox
            .items
            .iter()
            .filter(|i| !self.filter || i.kind.from_qass())
            .map(|i| i.slot.clone())
            .collect();
        if !self.filter {
            order.extend(inbox.with_qass.iter().map(|t| Slot::Agent(*t)));
            order.extend(inbox.finished.iter().map(|t| Slot::Agent(*t)));
        }
        order
    }

    /// The selected slot: the one chosen when it is still listed, else the first.
    fn selected(&self, order: &[Slot]) -> Option<Slot> {
        self.sel
            .as_ref()
            .filter(|s| order.contains(s))
            .or(order.first())
            .cloned()
    }

    fn select(&mut self, slot: Slot) {
        if self.sel.as_ref() != Some(&slot) {
            self.whole = false;
            self.stash_typing();
        }
        self.sel = Some(slot);
        self.follow.set(true);
    }

    fn step(&mut self, down: bool, env: &Env) {
        let order = self.order(&self.inbox(env));
        let Some(at) = self
            .selected(&order)
            .and_then(|s| order.iter().position(|o| *o == s))
        else {
            return;
        };
        let to = if down {
            (at + 1).min(order.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        self.select(order[to].clone());
    }

    /// The selected item, when it is one that needs the person.
    fn item(&self, inbox: &Inbox) -> Option<Item> {
        let order = self.order(inbox);
        let slot = self.selected(&order)?;
        inbox.items.iter().find(|i| i.slot == slot).cloned()
    }

    fn say(&mut self, what: impl Into<String>, env: &Env) {
        self.notice_slot = None;
        self.notice = Some((what.into(), env.now + NOTICE));
    }

    // Answers ------------------------------------------------------------------------------

    /// What an item offers, in the order its digits run.
    pub fn opts(item: &Item, env: &Env) -> Vec<Opt> {
        if item.lost {
            return Vec::new();
        }
        let go = || Opt {
            digit: Some(1),
            label: "Open the agent".into(),
            act: Act::Go,
        };
        match item.kind {
            // A multiple choice menu offers nothing here: the card says the question, and Open.
            ItemKind::Menu
                if item
                    .terminal
                    .and_then(|t| env.row(t))
                    .and_then(|r| r.menu.as_ref())
                    .is_some_and(|m| m.multi) =>
            {
                vec![go()]
            }
            ItemKind::Menu => item
                .terminal
                .and_then(|t| env.row(t))
                .and_then(|r| r.menu.as_ref())
                .map(offers)
                .unwrap_or_default()
                .into_iter()
                .filter(|o| o.pick != Pick::Qass)
                .map(
                    |Offer {
                         digit, label, pick, ..
                     }| Opt {
                        digit,
                        label,
                        act: Act::Pick(pick),
                    },
                )
                .collect(),
            ItemKind::Reply => Vec::new(),
            ItemKind::Dialog | ItemKind::Open | ItemKind::Failed => vec![go()],
            ItemKind::Qass | ItemKind::Agent | ItemKind::Plan => {
                let Slot::Card(id) = &item.slot else {
                    return Vec::new();
                };
                env.card(id)
                    .map(|c| {
                        c.body
                            .options
                            .iter()
                            .enumerate()
                            .map(|(i, o)| Opt {
                                digit: (i < 9).then_some(i as u8 + 1),
                                label: o.label.clone(),
                                act: Act::Pick(Pick::Card {
                                    card: id.clone(),
                                    option: o.key.clone(),
                                }),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
    }

    /// Whether the selected card takes the person's own words (`r` does something).
    pub fn can_type(&self, env: &Env) -> bool {
        self.item(&self.inbox(env))
            .is_some_and(|item| Self::takes_words(&item, env))
    }

    /// Whether the person can type words for an item: a menu with an answer field, an agent
    /// waiting for words, or a card.
    fn takes_words(item: &Item, env: &Env) -> bool {
        match item.kind {
            ItemKind::Menu => item
                .terminal
                .and_then(|t| env.row(t))
                .and_then(|r| r.menu.as_ref())
                .is_some_and(|m| !m.multi && text_key(m).is_some()),
            ItemKind::Reply | ItemKind::Qass | ItemKind::Agent | ItemKind::Plan => !item.lost,
            _ => false,
        }
    }

    /// Why an item takes no answer now, when it does not.
    fn closed(item: &Item, env: &Env) -> Option<String> {
        item.lost.then(|| {
            let machine = item
                .terminal
                .and_then(|t| env.row(t))
                .map_or("that computer".to_string(), |r| {
                    env.machine_name(r.workspace)
                });
            format!("{machine} is not reached, so this waits until it is back")
        })
    }

    fn start_hold(&mut self, hold: Hold) {
        self.said.remove(&hold.slot);
        self.holds.start(hold);
    }

    /// Answer `item` with its option `at`: held, or the agent opened.
    fn choose(&mut self, item: &Item, at: usize, env: &Env) -> Out {
        if let Some(why) = Self::closed(item, env) {
            self.say(why, env);
            return Out::Stay;
        }
        let Some(opt) = Self::opts(item, env).into_iter().nth(at) else {
            return Out::Stay;
        };
        let Act::Pick(pick) = opt.act else {
            return item.terminal.map_or(Out::Stay, Out::Jump);
        };
        let hold = match &item.slot {
            Slot::Agent(t) => {
                let Some(row) = env.row(*t) else {
                    return Out::Stay;
                };
                let Some(menu) = row.menu.as_ref() else {
                    return Out::Stay;
                };
                // A menu that has only just appeared takes no digit: a key meant for the
                // screen before it.
                if !self.holds.armed(*t, menu.seal, row.menu_seal, env.now) {
                    return Out::Stay;
                }
                Hold {
                    slot: item.slot.clone(),
                    generation: row.generation,
                    menu_seal: row.menu_seal,
                    seal: menu.seal,
                    pick,
                    label: opt.label,
                    until: env.now + HOLD,
                }
            }
            Slot::Card(id) => {
                let words = env.card(id).and_then(|c| match &pick {
                    Pick::Card { option, .. } => c
                        .body
                        .options
                        .iter()
                        .find(|o| o.key == *option)
                        .map(|o| o.words.clone()),
                    _ => None,
                });
                if let Some(words) = words {
                    self.seen_words.insert(item.slot.clone(), words);
                }
                Hold {
                    slot: item.slot.clone(),
                    generation: None,
                    menu_seal: None,
                    seal: 0,
                    pick,
                    label: opt.label,
                    until: env.now + HOLD,
                }
            }
        };
        self.start_hold(hold);
        Out::Stay
    }

    /// Hand `item` to qass, held like any answer.
    fn hand(&mut self, item: &Item, env: &Env) -> Out {
        if let Some(why) = Self::closed(item, env) {
            self.say(why, env);
            return Out::Stay;
        }
        if !item.hand_to_qass {
            let why = match item.kind {
                ItemKind::Qass | ItemKind::Plan => {
                    "qass asked this one, so it is yours to answer".to_string()
                }
                ItemKind::Menu | ItemKind::Agent => env
                    .hand
                    .qass_off()
                    .map_or("qass cannot take this one".to_string(), |why| {
                        format!("{why}, so it cannot take this")
                    }),
                _ => "only a question with answers can go to qass".to_string(),
            };
            self.say(why, env);
            return Out::Stay;
        }
        let hold = match &item.slot {
            Slot::Agent(t) => {
                let Some(row) = env.row(*t) else {
                    return Out::Stay;
                };
                Hold {
                    slot: item.slot.clone(),
                    generation: row.generation,
                    menu_seal: row.menu_seal,
                    seal: row.menu.as_ref().map_or(0, |m| m.seal),
                    pick: Pick::Qass,
                    label: "Hand to qass".into(),
                    until: env.now + HOLD,
                }
            }
            Slot::Card(id) => Hold {
                slot: item.slot.clone(),
                generation: None,
                menu_seal: None,
                seal: 0,
                pick: Pick::CardQass { card: id.clone() },
                label: "Hand to qass".into(),
                until: env.now + HOLD,
            },
        };
        self.start_hold(hold);
        Out::Stay
    }

    /// Start typing the person's own words for `item`.
    fn start_typing(&mut self, item: &Item, env: &Env) {
        if let Some(why) = Self::closed(item, env) {
            self.say(why, env);
            return;
        }
        if !Self::takes_words(item, env) {
            self.say("this one takes no typed answer, use its own answers", env);
            return;
        }
        self.notice = None;
        self.whole = false;
        self.text = self.drafts.remove(&item.slot).unwrap_or_default();
        self.typing = Some(item.slot.clone());
    }

    /// The typed words become a held answer.
    fn submit(&mut self, env: &Env) {
        let Some(slot) = self.typing.clone() else {
            return;
        };
        let text = self.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let inbox = self.inbox(env);
        let Some(item) = inbox.items.iter().find(|i| i.slot == slot) else {
            // Its question went while the words were typed: kept as a draft, nothing sent.
            self.say("that question is gone, your words are kept in the box", env);
            return;
        };
        let hold = match (&slot, item.kind) {
            (Slot::Agent(t), ItemKind::Menu) => {
                let (Some(row), Some(key)) = (
                    env.row(*t),
                    env.row(*t).and_then(|r| r.menu.as_ref()).and_then(text_key),
                ) else {
                    return;
                };
                Hold {
                    slot: slot.clone(),
                    generation: row.generation,
                    menu_seal: row.menu_seal,
                    seal: row.menu.as_ref().map_or(0, |m| m.seal),
                    pick: Pick::Text { key, text },
                    label: "your words".into(),
                    until: env.now + HOLD,
                }
            }
            (Slot::Agent(t), ItemKind::Reply) => {
                let Some(row) = env.row(*t) else {
                    return;
                };
                Hold {
                    slot: slot.clone(),
                    generation: row.generation,
                    menu_seal: None,
                    seal: 0,
                    pick: Pick::Reply {
                        text,
                        turn: row.stop.clone(),
                    },
                    label: "your words".into(),
                    until: env.now + HOLD,
                }
            }
            (Slot::Card(id), _) => Hold {
                slot: slot.clone(),
                generation: None,
                menu_seal: None,
                seal: 0,
                pick: Pick::CardText {
                    card: id.clone(),
                    text,
                },
                label: "your words".into(),
                until: env.now + HOLD,
            },
            _ => return,
        };
        self.start_hold(hold);
        self.typing = None;
        self.text.clear();
        self.drafts.remove(&slot);
    }

    /// Undo the held answer of `slot`, else the newest one (and what was held with it).
    fn undo(&mut self, slot: Option<&Slot>, env: &Env) {
        let slot = slot.filter(|s| self.holds.of(s).is_some());
        if self.holds.undo(slot) {
            self.say("undone, nothing was sent", env);
        }
    }

    /// Send the held answer of `slot` now, else the newest.
    fn send_now(&mut self, slot: Option<&Slot>, env: &Env) {
        let slot = slot.filter(|s| self.holds.of(s).is_some());
        let taken = self.holds.send_now(slot);
        self.send(taken, env);
    }

    /// Put `item` away unanswered: a card only, held like an answer so `u` undoes it.
    fn put_away(&mut self, item: &Item, env: &Env) {
        let Slot::Card(card) = &item.slot else {
            self.say("only a card can be put away", env);
            return;
        };
        self.start_hold(Hold {
            slot: item.slot.clone(),
            generation: None,
            menu_seal: None,
            seal: 0,
            pick: Pick::PutAway { card: card.clone() },
            label: "Put away".into(),
            until: env.now + HOLD,
        });
    }

    fn next_id(&mut self) -> String {
        self.writes += 1;
        format!("{ID}{}", self.writes)
    }

    // Sending ------------------------------------------------------------------------------

    /// Ask the daemon for what `holds` say: one request per pick, hand-offs one per machine.
    fn send(&mut self, holds: Vec<Hold>, env: &Env) {
        let mut hands: BTreeMap<usize, Vec<Hold>> = BTreeMap::new();
        for hold in holds {
            match &hold.pick {
                Pick::Qass => {
                    let machine = hold
                        .terminal()
                        .and_then(|t| env.row(t))
                        .map_or(0, |r| machine_of(r.workspace));
                    hands.entry(machine).or_default().push(hold);
                }
                Pick::Reply { text, .. } => {
                    let Some(terminal) = hold.terminal() else {
                        continue;
                    };
                    // Never into a line the person began in that terminal since the reply was
                    // held: the words go back to a draft on the card.
                    let began = hold.until.checked_sub(HOLD);
                    if env
                        .typed
                        .get(&terminal)
                        .is_some_and(|at| began.is_none_or(|b| *at > b))
                    {
                        self.drafts.insert(hold.slot.clone(), text.clone());
                        self.said.insert(
                            hold.slot.clone(),
                            "you typed in that terminal; your reply is kept here".into(),
                        );
                        continue;
                    }
                    self.out.typed.push(Typed {
                        terminal,
                        bytes: text.clone().into_bytes(),
                        paste: true,
                    });
                    self.out.typed.push(Typed {
                        terminal,
                        bytes: b"\r".to_vec(),
                        paste: false,
                    });
                    self.sent(&hold, env);
                }
                _ => self.send_one(hold, false),
            }
        }
        for (machine, held) in hands {
            let id = self.next_id();
            // Another machine's agent: for This Mac's qass, named by this Mac's hand id.
            let away = if machine == 0 {
                None
            } else {
                env.hand.hand_id.clone()
            };
            self.out.requests.push(ControlRequest::HandToQass {
                id: id.clone(),
                hands: held
                    .iter()
                    .filter_map(|h| {
                        Some(Hand {
                            terminal: h.terminal()?,
                            generation: h.generation,
                            seal: (h.seal != 0).then_some(h.seal),
                            away: away.clone(),
                        })
                    })
                    .collect(),
            });
            for hold in held {
                self.sending.push((id.clone(), hold, true));
            }
        }
    }

    fn send_one(&mut self, hold: Hold, retried: bool) {
        let id = self.next_id();
        let terminal = hold.terminal().unwrap_or_default();
        let card = |action: AskOp| ControlRequest::AskWrite {
            id: id.clone(),
            terminal: None,
            token: None,
            action,
        };
        let request = match &hold.pick {
            Pick::Key(key) => ControlRequest::AnswerMenu {
                id: id.clone(),
                terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
            },
            Pick::Text { key, text } => ControlRequest::AnswerMenuText {
                id: id.clone(),
                terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
                text: text.clone(),
            },
            Pick::Card { card: c, option } => card(AskOp::Answer {
                card: c.clone(),
                option: Some(option.clone()),
                text: None,
                seen: self.seen_words.remove(&hold.slot),
                to_qass: false,
            }),
            Pick::CardText { card: c, text } => card(AskOp::Answer {
                card: c.clone(),
                option: None,
                text: Some(text.clone()),
                seen: None,
                to_qass: false,
            }),
            Pick::CardQass { card: c } => card(AskOp::Answer {
                card: c.clone(),
                option: None,
                text: None,
                seen: None,
                to_qass: true,
            }),
            Pick::PutAway { card: c } => card(AskOp::Dismiss { card: c.clone() }),
            Pick::Qass | Pick::Reply { .. } => return,
        };
        self.out.requests.push(request);
        self.sending.push((id, hold, retried));
    }

    /// An answer reached the daemon: said on the notice line, and kept for Answered today.
    fn sent(&mut self, hold: &Hold, env: &Env) {
        self.said.remove(&hold.slot);
        let item_who = env.who_slot(&hold.slot);
        let said = match &hold.pick {
            Pick::Qass | Pick::CardQass { .. } => {
                format!("handed to qass, {}: {item_who}", clock(env.now_unix))
            }
            _ => format!("sent to {item_who}: {}", hold.label),
        };
        if matches!(hold.pick, Pick::PutAway { .. }) {
            self.say(format!("put away: {item_who}"), env);
            return;
        }
        self.say(said, env);
        self.notice_slot = Some(hold.slot.clone());
        self.own.push(OwnAnswer {
            at: env.now_unix,
            label: hold.label.clone(),
            to: item_who,
            card: match &hold.slot {
                Slot::Card(id) => Some(id.clone()),
                Slot::Agent(_) => None,
            },
        });
        if self.own.len() > OWN_MOST {
            self.own.remove(0);
        }
    }

    /// The daemon's answer to something this pane asked. True when the screen changed.
    pub fn answered(&mut self, response: &ControlResponse, env: &Env) -> bool {
        let id = response.request_id();
        // A daemon too old to read the request answers with no id: its refusal belongs to the
        // oldest answer asked under the variant it names.
        let at = if id.is_empty()
            && let ControlResponse::Failed { reason, .. } = response
        {
            self.sending
                .iter()
                .position(|(_, h, _)| refused_by(reason, h))
        } else {
            self.sending.iter().position(|(i, _, _)| i == id)
        };
        let Some(at) = at else {
            return false;
        };
        let wanted = self.sending[at].0.clone();
        let mut mine = Vec::new();
        self.sending.retain(|(i, h, retried)| {
            let hit = *i == wanted;
            if hit {
                mine.push((h.clone(), *retried));
            }
            !hit
        });
        for (hold, retried) in mine {
            // The menu it was for changed or is gone: what happened to it is not said under
            // whatever the agent asks now.
            let moved_on = hold.seal != 0
                && hold.terminal().and_then(|t| env.row(t)).is_none_or(|r| {
                    // The same question asked again is another menu: another turn seal, or
                    // another process.
                    r.menu.as_ref().is_none_or(|m| m.seal != hold.seal)
                        || r.generation != hold.generation
                        || matches!((hold.menu_seal, r.menu_seal), (Some(a), Some(b)) if a != b)
                });
            if moved_on {
                self.said.remove(&hold.slot);
                continue;
            }
            match response {
                ControlResponse::MenuAnswered { .. } | ControlResponse::AskWritten { .. } => {
                    self.sent(&hold, env);
                }
                ControlResponse::Handed { taken, .. } => {
                    if hold.terminal().is_some_and(|t| taken.contains(&t)) {
                        self.sent(&hold, env);
                    } else {
                        self.said.insert(
                            hold.slot.clone(),
                            "qass did not take this one: the question had moved on.".into(),
                        );
                    }
                }
                ControlResponse::Failed { reason, .. } => self.failed(hold, retried, reason, env),
                _ => {}
            }
        }
        true
    }

    fn failed(&mut self, hold: Hold, retried: bool, reason: &str, env: &Env) {
        // Keys still queued for the terminal: asked once more, a moment later.
        if reason == "busy" && !retried {
            self.retry.push((env.now + RETRY, hold));
            return;
        }
        let row = hold.terminal().and_then(|t| env.row(t));
        let machine = row.map_or_else(|| "This Mac".to_string(), |r| env.machine_name(r.workspace));
        let m = row.map_or(0, |r| machine_of(r.workspace));
        // A hand for This Mac's qass that the other machine answered as if it were for its own:
        // its qoop predates such hands, so it is not offered there again. Nothing is ever typed
        // into a pane the person did not choose.
        if crate::mux::refused_as_old(reason)
            && hold.pick == Pick::Qass
            && m != 0
            && env.hand.hand_id.is_some()
        {
            self.no_hands.insert(m);
        }
        let words = refusal_words(reason, &machine, &hold.pick);
        self.said.insert(hold.slot, words);
    }

    /// After a new agent list: a pick whose menu changed or went is dropped, and its card says
    /// who answered; the digits of a new menu wait out their arming. Called whenever the rows
    /// change, open or not.
    pub fn rows_changed(&mut self, env: &Env) {
        // A menu that comes up with another seal or turn than the last one seen for its agent,
        // the same text asked again included, is a new question: what was said of the answer to
        // the old one (a refusal, "sent to ...") does not stand under it.
        for row in env.rows.iter().filter(|r| r.menu.is_some()) {
            let Some(menu) = row.menu.as_ref() else {
                continue;
            };
            let key = (menu.seal, row.menu_seal);
            if self
                .turns
                .insert(row.terminal, key)
                .is_some_and(|old| old != key)
            {
                let slot = Slot::Agent(row.terminal);
                self.said.remove(&slot);
                if self.notice_slot.as_ref() == Some(&slot) {
                    self.notice = None;
                    self.notice_slot = None;
                }
            }
        }
        for hold in self.holds.menus(env.rows, env.now) {
            let by = hold
                .terminal()
                .and_then(|t| env.row(t))
                .and_then(|r| r.answered.as_ref())
                .map(|a| {
                    format!(
                        "Answered on the {}: {}.",
                        match a.by.as_str() {
                            "computer" => "Mac",
                            "phone" => "iPhone",
                            "attach" => "terminal",
                            other => other,
                        },
                        a.label
                    )
                });
            let words =
                by.unwrap_or_else(|| refusal_words("already_answered", "This Mac", &hold.pick));
            self.said.insert(hold.slot, words);
        }
    }

    /// The picks whose hold is up, and whatever is owed: asked of the daemon on the next
    /// [`Pane::take`]. True when the screen changed.
    pub fn tick(&mut self, env: &Env) -> bool {
        let mut moved = false;
        let due = self.holds.due(env.now);
        if !due.is_empty() {
            self.send(due, env);
            moved = true;
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retry)
            .into_iter()
            .partition(|(at, _)| *at <= env.now);
        self.retry = waiting;
        for (_, hold) in ready {
            self.send_one(hold, true);
            moved = true;
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, until)| *until <= env.now)
        {
            self.notice = None;
            moved = true;
        }
        // A hold counts down on screen, in the Inbox or on the status row: repainted when the
        // second changes.
        moved || !self.holds.held.is_empty()
    }

    /// Send what is on hold now, Undo or not: the interface is leaving.
    pub fn flush(&mut self, env: &Env) {
        let all = self.holds.flush();
        self.send(all, env);
        for (_, hold) in std::mem::take(&mut self.retry) {
            self.send_one(hold, true);
        }
    }

    /// Whether something is waiting to be sent.
    pub fn owes(&self) -> bool {
        !self.out.is_empty()
    }

    /// What to send now.
    pub fn take(&mut self) -> Sends {
        std::mem::take(&mut self.out)
    }

    /// The moment the pane next has something to do by itself: a write to send, a hold that
    /// is due or whose count changes, a retry, a notice to let go.
    pub fn wake_at(&self, now: Instant) -> Option<Instant> {
        if !self.out.is_empty() {
            return Some(now);
        }
        let mut at: Option<Instant> = None;
        let mut soon = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        for hold in &self.holds.held {
            let left = hold.until.saturating_duration_since(now);
            // The next whole second boundary of the count, or the end of the hold.
            let tick = match left.subsec_nanos() {
                0 => Duration::from_secs(1),
                n => Duration::from_nanos(u64::from(n) + 1_000_000),
            };
            soon(now + tick.min(left.max(Duration::from_millis(1))));
        }
        for (when, _) in &self.retry {
            soon(*when);
        }
        if let Some((_, until)) = &self.notice {
            soon(*until);
        }
        at
    }

    /// The line a hold leaves on the status row while the Inbox is put away: what is held, for
    /// how long, and how to undo it. `None` with nothing held.
    pub fn held_line(&self, env: &Env) -> Option<String> {
        let hold = self.holds.held.last()?;
        let left = hold.until.saturating_duration_since(env.now);
        Some(crate::keys::named(&format!(
            "{}  ctrl-a i then u undoes",
            hold_words(
                hold,
                left.as_secs_f64().ceil().max(1.0) as u64,
                &env.who_slot(&hold.slot)
            )
        )))
    }

    // Keys ---------------------------------------------------------------------------------

    /// A key, or the bytes of several. `prefix` is the interface's prefix byte, which with a
    /// mouse report or a detach is not the Inbox's and goes back.
    pub fn key(&mut self, bytes: &[u8], prefix: u8, env: &Env) -> Out {
        // From the prefix, a mouse report or a detach on, the bytes are the ordinary path's,
        // wherever in the read they come: what came before them is the Inbox's.
        let at = (0..bytes.len())
            .find(|&i| bytes[i] == prefix || bytes[i] == 0x11 || bytes[i..].starts_with(b"\x1b[<"));
        let (mine, rest) = match at {
            Some(i) => (&bytes[..i], Some(bytes[i..].to_vec())),
            None => (bytes, None),
        };
        match (self.own_keys(mine, env), rest) {
            (Out::Stay, Some(rest)) => Out::Pass(rest),
            (out, _) => out,
        }
    }

    fn own_keys(&mut self, bytes: &[u8], env: &Env) -> Out {
        if self.typing.is_some() {
            return self.type_key(bytes, env);
        }
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            let (key, used) = if rest.starts_with(b"\x1b[A") || rest.starts_with(b"\x1bOA") {
                (Key::Up, 3)
            } else if rest.starts_with(b"\x1b[B") || rest.starts_with(b"\x1bOB") {
                (Key::Down, 3)
            } else if rest == b"\x1b" {
                (Key::Esc, 1)
            } else if rest[0] == 0x1b {
                // Some other sequence: not ours, and not worth reading as a letter.
                return Out::Stay;
            } else {
                (Key::Char(rest[0]), 1)
            };
            i += used;
            match self.act(key, env) {
                Out::Stay => {}
                other => return other,
            }
        }
        Out::Stay
    }

    fn act(&mut self, key: Key, env: &Env) -> Out {
        let inbox = self.inbox(env);
        let item = self.item(&inbox);
        let order = self.order(&inbox);
        let slot = self.selected(&order);
        match key {
            Key::Esc if self.whole => {
                self.whole = false;
                Out::Stay
            }
            Key::Esc => {
                if self.notice.take().is_some()
                    || slot.as_ref().is_some_and(|s| self.said.remove(s).is_some())
                {
                    return Out::Stay;
                }
                Out::Closed
            }
            Key::Char(b'j') | Key::Down if self.whole => {
                self.follow.set(false);
                self.offset
                    .set((self.offset.get() + 1).min(self.max_offset.get()));
                Out::Stay
            }
            Key::Char(b'k') | Key::Up if self.whole => {
                self.follow.set(false);
                self.offset.set(self.offset.get().saturating_sub(1));
                Out::Stay
            }
            Key::Char(b'j') | Key::Down => {
                self.step(true, env);
                Out::Stay
            }
            Key::Char(b'k') | Key::Up => {
                self.step(false, env);
                Out::Stay
            }
            Key::Char(d @ b'1'..=b'9') => {
                let Some(item) = item else {
                    return self.go_quiet(slot.as_ref(), d, env);
                };
                match Self::opts(&item, env)
                    .iter()
                    .position(|o| o.digit == Some(d - b'0'))
                {
                    Some(at) => self.choose(&item, at, env),
                    None => Out::Stay,
                }
            }
            Key::Char(b'u') => {
                self.undo(slot.as_ref(), env);
                Out::Stay
            }
            Key::Char(b'\r' | b'\n') => {
                // Send now: the selected card's hold, else the newest (the bar says so).
                if !self.holds.held.is_empty() {
                    self.send_now(slot.as_ref(), env);
                    return Out::Stay;
                }
                match (&item, &slot) {
                    (Some(item), _)
                        if matches!(
                            item.kind,
                            ItemKind::Dialog | ItemKind::Open | ItemKind::Failed
                        ) =>
                    {
                        item.terminal.map_or(Out::Stay, Out::Jump)
                    }
                    (None, Some(Slot::Agent(t))) => Out::Jump(*t),
                    _ => Out::Stay,
                }
            }
            Key::Char(b'h' | b'q') => match item {
                Some(item) => self.hand(&item, env),
                None => Out::Stay,
            },
            Key::Char(b'r') => {
                if let Some(item) = item {
                    self.start_typing(&item, env);
                }
                Out::Stay
            }
            Key::Char(b'g') => match (&item, &slot) {
                (Some(item), _) => item.terminal.map_or(Out::Stay, Out::Jump),
                (None, Some(Slot::Agent(t))) => Out::Jump(*t),
                _ => Out::Stay,
            },
            Key::Char(b'o') => match item {
                Some(Item {
                    slot: Slot::Card(id),
                    ..
                }) => env
                    .card(&id)
                    .map_or(Out::Stay, |c| Out::Talk(Box::new(c.clone()))),
                Some(item) => item.terminal.map_or(Out::Stay, Out::Jump),
                None => Out::Stay,
            },
            Key::Char(b'm') => {
                self.whole = !self.whole && item.is_some();
                self.offset.set(0);
                self.follow.set(false);
                Out::Stay
            }
            Key::Char(b'x') => {
                if let Some(item) = item {
                    self.put_away(&item, env);
                }
                Out::Stay
            }
            Key::Char(b'f') => {
                self.filter = !self.filter;
                self.follow.set(true);
                self.sel = None;
                Out::Stay
            }
            // `n` (a new agent) and tab (the Board) are not here yet: taken, so they type
            // nothing into the agent behind.
            Key::Char(_) => Out::Stay,
        }
    }

    /// A digit on a quiet row (with qass, finished): `1` opens its agent.
    fn go_quiet(&mut self, slot: Option<&Slot>, digit: u8, env: &Env) -> Out {
        match (slot, digit) {
            (Some(Slot::Agent(t)), b'1') => Out::Jump(*t),
            _ => {
                self.say("nothing to answer here", env);
                Out::Stay
            }
        }
    }

    fn type_key(&mut self, bytes: &[u8], env: &Env) -> Out {
        if bytes == b"\x1b" {
            self.stash_typing();
            return Out::Stay;
        }
        if bytes == b"\x15" {
            self.text.clear();
            return Out::Stay;
        }
        // A paste's brackets are not letters; its newlines are spaces, never an enter.
        let pasted = bytes.starts_with(b"\x1b[200~");
        let body: &[u8] = if pasted {
            bytes
                .strip_prefix(b"\x1b[200~")
                .map(|b| b.strip_suffix(b"\x1b[201~").unwrap_or(b))
                .unwrap_or(bytes)
        } else {
            bytes
        };
        // An arrow or a delete is not a letter either.
        if !pasted && body.len() > 1 && body[0] == 0x1b {
            return Out::Stay;
        }
        for c in String::from_utf8_lossy(body).chars() {
            match c {
                '\r' | '\n' if !pasted => {
                    self.submit(env);
                    return Out::Stay;
                }
                '\r' | '\n' => self.push(' '),
                '\u{7f}' | '\u{8}' => {
                    self.text.pop();
                }
                c if c.is_control() => {}
                c => self.push(c),
            }
        }
        Out::Stay
    }

    fn push(&mut self, c: char) {
        if self.text.chars().count() < TEXT_MAX {
            self.text.push(c);
        }
    }

    // Mouse --------------------------------------------------------------------------------

    /// A click on something the view drew.
    pub fn click(&mut self, hit: Hit, env: &Env) -> Out {
        let inbox = self.inbox(env);
        let item_of = |slot: &Slot| inbox.items.iter().find(|i| i.slot == *slot).cloned();
        match hit {
            Hit::Item(slot) => self.select(slot),
            Hit::Opt(slot, at) => {
                self.select(slot.clone());
                if let Some(item) = item_of(&slot) {
                    return self.choose(&item, at, env);
                }
            }
            Hit::Hand(slot) => {
                self.select(slot.clone());
                if let Some(item) = item_of(&slot) {
                    return self.hand(&item, env);
                }
            }
            Hit::Reply(slot) => {
                self.select(slot.clone());
                if let Some(item) = item_of(&slot) {
                    self.start_typing(&item, env);
                }
            }
            Hit::Undo(slot) => self.undo(Some(&slot), env),
            Hit::SendNow(slot) => self.send_now(Some(&slot), env),
            Hit::Filter => {
                self.filter = !self.filter;
                self.sel = None;
                self.follow.set(true);
            }
            Hit::Go(terminal) => return Out::Jump(terminal),
        }
        Out::Stay
    }

    /// The wheel: three rows a notch, the view no longer following the selection.
    pub fn scroll(&mut self, down: bool) {
        self.follow.set(false);
        let off = self.offset.get();
        self.offset.set(if down {
            (off + 3).min(self.max_offset.get())
        } else {
            off.saturating_sub(3)
        });
    }

    /// Whether the person is typing their own words.
    pub fn typing(&self) -> bool {
        self.typing.is_some()
    }
}

/// The request variant a pick is asked as, for a refusal that names one.
fn variant(hold: &Hold) -> &'static str {
    match hold.pick {
        Pick::Key(_) => "AnswerMenu",
        Pick::Text { .. } => "AnswerMenuText",
        Pick::Qass => "HandToQass",
        Pick::Card { .. }
        | Pick::CardText { .. }
        | Pick::CardQass { .. }
        | Pick::PutAway { .. }
        | Pick::Reply { .. } => "AskWrite",
    }
}

/// Whether `reason`, which names no request, is the refusal of `hold`'s: a daemon too old to
/// read the request names the variant it did not know. (Behind the mux the refusal comes with
/// the id of the request, which the mux keeps by its variant.)
fn refused_by(reason: &str, hold: &Hold) -> bool {
    qoop_runtime::control::too_old_for(reason, Some(variant(hold)))
}

/// What a hold says while it waits, as the phone says it: a hand-off "handing to qass: agent,
/// sending in 3 s", any other answer "sending in 3 s: Yes to agent".
fn hold_words(hold: &Hold, secs: u64, who: &str) -> String {
    match hold.pick {
        Pick::Qass | Pick::CardQass { .. } => {
            format!("handing to qass: {who}, sending in {secs} s")
        }
        _ => format!("sending in {secs} s: {} to {who}", hold.label),
    }
}

/// The person's local wall clock at `unix`, "16:04".
fn clock(unix: i64) -> String {
    let at = (qoop_runtime::schedule::system_zone().local)(unix);
    format!("{:02}:{:02}", at.hour, at.minute)
}

/// Whom the next item to go to is about, and what it is doing as a whole predicate, for the
/// status line ("who has failed"): the agent is named as the agents column names it, its
/// space and its session, so one agent reads the same in both places.
pub fn next_words(item: &Item, env: &Env) -> (String, String, &'static str) {
    let says = match item.kind {
        ItemKind::Failed => "has failed",
        ItemKind::Menu | ItemKind::Reply | ItemKind::Dialog | ItemKind::Open => "is blocked",
        ItemKind::Qass | ItemKind::Agent | ItemKind::Plan => "is asking",
    };
    let who = item
        .terminal
        .and_then(|t| env.row(t))
        .map(|row| {
            format!(
                "{} {}",
                env.space_name(row.workspace),
                crate::sidebar::session_name(row)
            )
        })
        .unwrap_or_else(|| env.who(item));
    (who, env.who(item), says)
}

/// What the client knows about qass and the hands to it, read off its lists: This Mac's qass
/// (the first agent in its space), its switch, the machines that refused a hand as too old, the
/// remote machines' names, this Mac's hand id (read once, when a hand from or to another machine
/// can matter), and the hands noted at `now`. Idempotent: called with every new list and before
/// every frame.
pub fn update_hand(
    hand: &mut HandInput,
    spaces: &[WorkspaceRow],
    rows: &[AgentRow],
    cards: &[AskInfo],
    no_hands: &BTreeSet<usize>,
    watching: Option<bool>,
    now: i64,
) {
    let qass_spaces: BTreeSet<i64> = spaces
        .iter()
        .filter(|w| w.name == crate::sidebar::QASS_SPACE && machine_of(w.id) == 0)
        .map(|w| w.id)
        .collect();
    hand.qass = rows
        .iter()
        .find(|r| r.agent && qass_spaces.contains(&r.workspace))
        .map(|r| (r.terminal, r.generation));
    hand.watching = watching;
    hand.no_hands = no_hands.clone();
    hand.machines = spaces
        .iter()
        .filter(|w| machine_of(w.id) != 0)
        .filter_map(|w| {
            let (machine, _) = w.name.split_once(':')?;
            Some((machine_of(w.id), machine.to_string()))
        })
        .collect();
    if hand.hand_id.is_none()
        && rows
            .iter()
            .any(|r| r.qass_away.is_some() || machine_of(r.workspace) != 0)
    {
        hand.hand_id = qoop_runtime::paths::hand_id().ok();
    }
    hand.set_carded(cards);
    crate::answers::note_qass(hand, rows, now);
}

/// What the Inbox needs around the rows: when the person's day began, the machines not
/// reached, the machines whose qoop sends no menu, qass's spaces.
pub fn context(spaces: &[WorkspaceRow], day_start: i64, old: BTreeSet<usize>) -> Context {
    Context {
        day_start,
        lost: spaces
            .iter()
            .filter(|w| w.link == Some(qoop_runtime::control::LinkState::Down))
            .map(|w| machine_of(w.id))
            .collect(),
        old,
        qass_spaces: spaces
            .iter()
            .filter(|w| w.name == crate::sidebar::QASS_SPACE)
            .map(|w| w.id)
            .collect(),
    }
}

/// When the person's local day began, kept in `cache` while `now` is inside it: a day is never
/// shorter than 23 hours, so the start is looked up again only after that (and when the clock
/// is set back).
pub fn day_start(cache: &mut i64, now: i64) -> i64 {
    if now < *cache || now - *cache >= 23 * 3600 {
        let zone = qoop_runtime::schedule::system_zone();
        let wall = (zone.local)(now);
        *cache = qoop_runtime::schedule::instant_of(
            zone,
            qoop_runtime::schedule::Naive::new(wall.year, wall.month, wall.day, 0, 0),
        );
    }
    *cache
}

enum Key {
    Esc,
    Up,
    Down,
    Char(u8),
}

// View -----------------------------------------------------------------------------------

/// One row of the pane before it is painted: its pieces, and where each click lands.
#[derive(Clone, Debug, Default)]
struct Row {
    segs: Vec<Seg>,
    hits: Vec<(usize, usize, Hit)>,
}

impl Row {
    fn plain(role: Role, text: impl Into<String>) -> Row {
        Row {
            segs: vec![seg(role, text)],
            hits: Vec::new(),
        }
    }

    fn blank() -> Row {
        Row::plain(Role::Text, "")
    }
}

/// A piece that is never split across two rows (an option with its number, a key with its
/// word), and what a click on it does.
struct Unit {
    role: Role,
    bold: bool,
    text: String,
    hit: Option<Hit>,
}

fn unit(role: Role, text: impl Into<String>, hit: Option<Hit>) -> Unit {
    Unit {
        role,
        bold: false,
        text: text.into(),
        hit,
    }
}

/// `units` laid out in rows of `cols` columns behind `indent` blank columns, `gap` blank
/// columns between two, breaking only between units; a unit wider than the row is wrapped on
/// its words.
fn flow(units: Vec<Unit>, indent: usize, gap: usize, cols: usize) -> Vec<Row> {
    let room = cols.saturating_sub(indent).max(1);
    let mut rows: Vec<Row> = Vec::new();
    let mut used = 0usize;
    let mut row = Row::plain(Role::Text, " ".repeat(indent));
    let mut empty = true;
    let mut x = indent;
    let push_piece = |row: &mut Row, x: &mut usize, u: &Unit, text: &str| {
        let w = width::width(text);
        row.segs.push(Seg {
            role: u.role,
            bold: u.bold,
            text: text.to_string(),
        });
        if let Some(hit) = &u.hit {
            row.hits.push((*x, *x + w, hit.clone()));
        }
        *x += w;
        w
    };
    for u in &units {
        let w = width::width(&u.text);
        if w > room {
            if !empty {
                rows.push(std::mem::replace(
                    &mut row,
                    Row::plain(Role::Text, " ".repeat(indent)),
                ));
            }
            for piece in wrap_whole(&u.text, room) {
                x = indent;
                push_piece(&mut row, &mut x, u, &piece);
                rows.push(std::mem::replace(
                    &mut row,
                    Row::plain(Role::Text, " ".repeat(indent)),
                ));
            }
            used = 0;
            x = indent;
            empty = true;
            continue;
        }
        if !empty && used + gap + w > room {
            rows.push(std::mem::replace(
                &mut row,
                Row::plain(Role::Text, " ".repeat(indent)),
            ));
            used = 0;
            x = indent;
            empty = true;
        }
        if !empty {
            row.segs.push(seg(Role::Text, " ".repeat(gap)));
            used += gap;
            x += gap;
        }
        used += push_piece(&mut row, &mut x, u, &u.text);
        empty = false;
    }
    if !empty || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// `text` wrapped whole behind `indent` columns of `cols`, in one ink.
fn wrapped(text: &str, indent: usize, cols: usize, role: Role, bold_it: bool) -> Vec<Row> {
    let room = cols.saturating_sub(indent).max(1);
    let lead = " ".repeat(indent);
    wrap_whole(text, room)
        .into_iter()
        .map(|piece| Row {
            segs: vec![
                seg(Role::Text, lead.clone()),
                Seg {
                    role,
                    bold: bold_it,
                    text: piece,
                },
            ],
            hits: Vec::new(),
        })
        .collect()
}

/// How long ago, in the words a card uses.
fn since(env: &Env, at: Option<i64>) -> Option<String> {
    let secs = env.now_unix - at?;
    Some(crate::asks::age_short(secs))
}

impl Pane {
    /// The Inbox painted into `cols` by `rows`, from `env`.
    pub fn view(&self, env: &Env, cols: usize, rows: usize) -> View {
        let inbox = self.inbox(env);
        let order = self.order(&inbox);
        let selected = self.selected(&order);

        // The top: the title and the one count, then a rule; a computer not reached says so.
        let mut head: Vec<Row> = Vec::new();
        head.extend(self.header(&inbox, env, cols));
        let rule_at = head.len();
        head.push(Row::blank());
        for machine in &env.ctx.lost {
            let name = env
                .spaces
                .iter()
                .find(|s| machine_of(s.id) == *machine)
                .map_or_else(|| "a computer".to_string(), |s| env.machine_name(s.id));
            head.extend(wrapped(
                &format!("{name} is not reached: what shows from it is as of its last answer"),
                1,
                cols,
                Role::Blocked,
                false,
            ));
        }

        // The bottom: a hold's words or the typed ones, the notice, the keys.
        let mut foot: Vec<Row> = Vec::new();
        let mut cursor: Option<(usize, usize)> = None;
        let mut typed_at = 0;
        let item = selected
            .as_ref()
            .and_then(|s| inbox.items.iter().find(|i| i.slot == *s));
        if let Some(slot) = &self.typing {
            let who = inbox
                .items
                .iter()
                .find(|i| i.slot == *slot)
                .map_or(String::new(), |i| env.who(i));
            foot.extend(wrapped(
                &format!("your words for {who}"),
                1,
                cols,
                Role::Focused,
                true,
            ));
            typed_at = foot.len();
            let line = format!("> {}", self.text);
            let typed = wrapped(&line, 1, cols, Role::Text, false);
            foot.extend(typed);
            foot.extend(wrapped(
                "enter holds it 3 s, then sends   esc keeps it as a draft",
                1,
                cols,
                Role::FaintText,
                false,
            ));
        } else {
            if let Some((said, _)) = &self.notice {
                foot.extend(wrapped(said, 1, cols, Role::Done, false));
            }
            foot.extend(flow(self.hints(item, env), 1, 2, cols));
        }

        let height = rows.saturating_sub(head.len() + foot.len()).max(1);
        let (body, range) = if self.whole {
            (self.whole_rows(item, env, cols), None)
        } else {
            self.body(&inbox, &order, &selected, env, cols)
        };

        // Scrolled to keep the selected card on show, or where the wheel left it.
        let max = body.len().saturating_sub(height);
        let mut off = self.offset.get().min(max);
        if self.follow.get()
            && let Some((start, end)) = range
        {
            if start < off {
                off = start;
            } else if end > off + height {
                off = end.saturating_sub(height).min(start);
            }
        }
        self.offset.set(off);
        self.max_offset.set(max);

        // The rule says when more is out of sight.
        let more = match (off > 0, off < max) {
            (true, true) => "more above and below",
            (true, false) => "more above",
            (false, true) => "more below",
            (false, false) => "",
        };
        head[rule_at] = rule_row(cols, more);

        let mut shown: Vec<Row> = head;
        shown.extend(
            body.into_iter()
                .skip(off)
                .chain(std::iter::repeat(Row::blank()))
                .take(height),
        );
        let foot_at = shown.len();
        shown.extend(foot);
        if self.typing.is_some() {
            // The caret sits at the end of the typed words, on the last row of the box.
            let typed_rows = wrap_whole(&format!("> {}", self.text), cols.saturating_sub(1).max(1));
            let last = typed_rows.last().map_or(0, |r| width::width(r));
            cursor = Some((
                foot_at + typed_at + typed_rows.len().saturating_sub(1),
                1 + last,
            ));
        }
        shown.truncate(rows.max(1));

        let mut targets = Vec::new();
        let mut painted = Vec::new();
        let mut plain = Vec::new();
        for (at, row) in shown.iter().enumerate() {
            for (x0, x1, hit) in &row.hits {
                targets.push(Target {
                    row: at,
                    x0: *x0,
                    x1: *x1,
                    hit: hit.clone(),
                });
            }
            painted.push(line(&row.segs, cols, false));
            let text: String = row.segs.iter().map(|s| s.text.as_str()).collect();
            plain.push(width::fit(&text, cols));
        }
        View {
            rows: painted,
            plain,
            targets,
            cursor,
        }
    }

    /// The title row: the one count, qass, the From qass chip.
    fn header(&self, inbox: &Inbox, env: &Env, cols: usize) -> Vec<Row> {
        let n = inbox.count;
        let from_qass = inbox.items.iter().filter(|i| i.kind.from_qass()).count();
        let qass = match env.hand.qass_off() {
            None => "\u{25c6} qass watching",
            Some(why) if why.contains("watching") => "\u{25c6} qass not watching",
            Some(_) => "\u{25c6} qass not running",
        };
        let mut units = vec![Unit {
            role: Role::Title,
            bold: true,
            text: "inbox".into(),
            hit: None,
        }];
        units.push(if n > 0 {
            Unit {
                role: Role::Blocked,
                bold: true,
                text: format!("needs you {n}"),
                hit: None,
            }
        } else {
            unit(Role::FaintText, "nothing needs you", None)
        });
        units.push(Unit {
            role: if self.filter {
                Role::Focused
            } else {
                Role::FaintText
            },
            bold: self.filter,
            text: if self.filter {
                format!("from qass {from_qass} (f shows all)")
            } else {
                format!("from qass {from_qass}")
            },
            hit: Some(Hit::Filter),
        });
        units.push(unit(Role::FaintText, qass, None));
        flow(units, 1, 3, cols)
    }

    /// The keys that work now, as the footer lists them.
    fn hints(&self, item: Option<&Item>, env: &Env) -> Vec<Unit> {
        let faint = |t: &str| unit(Role::FaintText, t, None);
        let mut out = Vec::new();
        if self.whole {
            let mut out = vec![faint("j k scroll")];
            if !self.holds.held.is_empty() {
                out.push(faint("u undo"));
                out.push(faint("enter send now"));
            }
            out.push(faint("m or esc back to the list"));
            return out;
        }
        if !self.holds.held.is_empty() {
            out.push(faint("u undo"));
            out.push(faint("enter send now"));
        }
        if let Some(item) = item {
            let opts = Self::opts(item, env);
            if opts.iter().any(|o| o.digit.is_some()) {
                out.push(faint("1-9 answer"));
            }
            if item.hand_to_qass {
                out.push(faint("h hand to qass"));
            }
            if Self::takes_words(item, env) {
                out.push(faint("r reply"));
            }
            if item.terminal.is_some() {
                out.push(faint("g go to the agent"));
            }
            out.push(faint("o talk it through"));
            out.push(faint("m read whole"));
            if matches!(item.slot, Slot::Card(_)) {
                out.push(faint("x put away"));
            }
        }
        out.push(faint("j k move"));
        out.push(faint(if self.filter {
            "f show all"
        } else {
            "f from qass"
        }));
        out.push(faint("esc back"));
        out
    }

    /// The list: a card for each item, then the quiet sections. The rows of the selected one,
    /// as a start and an end, for the scroll to keep on show.
    fn body(
        &self,
        inbox: &Inbox,
        order: &[Slot],
        selected: &Option<Slot>,
        env: &Env,
        cols: usize,
    ) -> (Vec<Row>, Option<(usize, usize)>) {
        let mut body: Vec<Row> = Vec::new();
        let mut range = None;
        let items: Vec<&Item> = inbox
            .items
            .iter()
            .filter(|i| order.contains(&i.slot))
            .collect();
        if items.is_empty() {
            body.push(Row::plain(
                Role::FaintText,
                if self.filter {
                    " nothing from qass needs you"
                } else {
                    " nothing needs you"
                },
            ));
            body.push(Row::blank());
        }
        for item in items {
            let on = selected.as_ref() == Some(&item.slot);
            let start = body.len();
            body.extend(self.card(item, on, env, cols));
            if on {
                range = Some((start, body.len()));
            }
        }
        if !self.filter {
            let quiet = |title: &str, n: usize| -> Row {
                Row::plain(Role::FaintText, format!(" {title} {n}"))
            };
            if !inbox.with_qass.is_empty() {
                body.push(quiet("with qass", inbox.with_qass.len()));
                for t in &inbox.with_qass {
                    let on = selected.as_ref() == Some(&Slot::Agent(*t));
                    let start = body.len();
                    body.extend(self.quiet(*t, on, "\u{25c6} ", "", env, cols));
                    if on {
                        range = Some((start, body.len()));
                    }
                }
                body.push(Row::blank());
            }
            if !inbox.finished.is_empty() {
                body.push(quiet("finished", inbox.finished.len()));
                for t in &inbox.finished {
                    let on = selected.as_ref() == Some(&Slot::Agent(*t));
                    let start = body.len();
                    body.extend(self.quiet(*t, on, "", "finished, not read", env, cols));
                    if on {
                        range = Some((start, body.len()));
                    }
                }
                body.push(Row::blank());
            }
        }
        if !inbox.answered.is_empty() {
            body.push(Row::plain(
                Role::FaintText,
                format!(" answered today {}", inbox.answered.len()),
            ));
            for a in &inbox.answered {
                let ago = crate::asks::age_short(env.now_unix - a.at);
                body.extend(wrapped(
                    &format!("{} to {}, {ago} ago", a.label, a.to),
                    3,
                    cols,
                    Role::FaintText,
                    false,
                ));
            }
        }
        (body, range)
    }

    /// A row of With qass or Finished: the agent, one line wrapped whole, a click goes there.
    fn quiet(&self, t: u32, on: bool, mark: &str, note: &str, env: &Env, cols: usize) -> Vec<Row> {
        let Some(row) = env.row(t) else {
            return Vec::new();
        };
        let mut text = format!(
            "{} / {}",
            env.space_name(row.workspace),
            crate::sidebar::label(row)
        );
        if !note.is_empty() {
            text = format!("{text}  {note}");
        }
        let lead = if on { " \u{258c} " } else { "   " };
        let mut rows = wrapped(
            &format!("{mark}{text}"),
            TITLE,
            cols,
            Role::FaintText,
            false,
        );
        for (k, r) in rows.iter_mut().enumerate() {
            r.segs[0] = seg(
                if on { Role::Blocked } else { Role::Text },
                if k == 0 { lead } else { "   " },
            );
            r.hits.push((0, cols, Hit::Go(t)));
        }
        rows
    }

    /// One item as its card: who and how long, the question whole, what is held or refused,
    /// the answers wrapped at their boundaries, then a blank row.
    fn card(&self, item: &Item, on: bool, env: &Env, cols: usize) -> Vec<Row> {
        let slot = item.slot.clone();
        let mut rows = self.title_rows(item, on, env, cols);
        rows.extend(self.question_rows(item, on, env, cols));
        if let Some(why) = Self::closed(item, env) {
            rows.extend(wrapped(&why, BODY, cols, Role::FaintText, false));
        }
        rows.extend(self.answer_rows(item, on, env, cols));
        rows.extend(self.hold_rows(&slot, env, cols));
        if let Some(words) = self.said.get(&slot) {
            rows.extend(wrapped(words, BODY, cols, Role::Failed, true));
        }
        rows.push(Row::blank());
        // Any cell of the card selects it: after the specific hits, so those win.
        for r in &mut rows {
            r.hits.push((0, cols, Hit::Item(slot.clone())));
        }
        rows
    }

    /// The title, whole, with how long it has waited. qass's own notice names no agent.
    fn title_rows(&self, item: &Item, on: bool, env: &Env, cols: usize) -> Vec<Row> {
        let who = env.who(item);
        let mark = if item.kind.from_qass() {
            "\u{25c6} via qass  "
        } else {
            ""
        };
        let title = if who == "qass" && !mark.is_empty() {
            "\u{25c6} qass".to_string()
        } else {
            format!("{mark}{who}")
        };
        let lead_ink = if item.kind == ItemKind::Failed {
            Role::Failed
        } else {
            Role::Blocked
        };
        let mut titled = wrapped(&title, TITLE, cols, Role::Text, true);
        for (k, r) in titled.iter_mut().enumerate() {
            r.segs[0] = if k == 0 && on {
                bold(lead_ink, " \u{258c} ")
            } else {
                seg(Role::Text, "   ")
            };
        }
        let age = since(env, item.since).unwrap_or_default();
        if !age.is_empty()
            && let Some(last) = titled.last_mut()
        {
            let used: usize = last.segs.iter().map(|s| width::width(&s.text)).sum();
            let tail = format!("  {age}");
            if used + width::width(&tail) <= cols {
                last.segs.push(seg(Role::FaintText, tail));
            } else {
                titled.push(Row {
                    segs: vec![seg(Role::Text, "   "), seg(Role::FaintText, age)],
                    hits: Vec::new(),
                });
            }
        }
        titled
    }

    /// The question, whole, and on the selected card what more the person needs to decide: the
    /// command, why.
    fn question_rows(&self, item: &Item, on: bool, env: &Env, cols: usize) -> Vec<Row> {
        let row = item.terminal.and_then(|t| env.row(t));
        let card = match &item.slot {
            Slot::Card(id) => env.card(id),
            Slot::Agent(_) => None,
        };
        let text = match item.kind {
            ItemKind::Menu => row
                .and_then(|r| r.menu.as_ref())
                .map(|m| {
                    if m.question.is_empty() {
                        "It stopped on a choice.".to_string()
                    } else {
                        m.question.clone()
                    }
                })
                .unwrap_or_default(),
            ItemKind::Reply => "It stopped and waits for your words.".to_string(),
            // The daemon's draft card says what is on its screen; with none, what is known.
            ItemKind::Dialog => card.map_or_else(
                || "It has a dialog qoop cannot read.".to_string(),
                |c| c.body.asks.clone(),
            ),
            ItemKind::Open => {
                "It is blocked, and qoop on its computer is too old to show what it asks."
                    .to_string()
            }
            ItemKind::Failed => "It failed.".to_string(),
            ItemKind::Qass | ItemKind::Agent | ItemKind::Plan => {
                card.map(|c| c.body.asks.clone()).unwrap_or_default()
            }
        };
        let mut rows = wrapped(&text, BODY, cols, Role::Text, false);
        if on {
            let more: Vec<String> = match (item.kind, row, card) {
                // The prompt above the options as the agent drew it; the question itself is
                // already said, so a line that only repeats it is left out.
                (ItemKind::Menu, Some(r), _) => r
                    .menu
                    .as_ref()
                    .map(|m| {
                        m.context
                            .iter()
                            .filter(|l| l.trim() != m.question.trim())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                (_, _, Some(c)) => vec![
                    format!("doing: {}", c.body.doing),
                    format!("why: {}", c.body.why),
                ],
                (ItemKind::Failed | ItemKind::Dialog | ItemKind::Open, Some(r), _) => {
                    r.activity.iter().map(|a| format!("it was: {a}")).collect()
                }
                _ => Vec::new(),
            };
            for line in more {
                rows.extend(wrapped(&line, BODY, cols, Role::FaintText, false));
            }
        }
        rows
    }

    /// The answers, wrapped at their boundaries, then the person's own draft if one is kept.
    fn answer_rows(&self, item: &Item, on: bool, env: &Env, cols: usize) -> Vec<Row> {
        let slot = &item.slot;
        let card = match slot {
            Slot::Card(id) => env.card(id),
            Slot::Agent(_) => None,
        };
        let mut units: Vec<Unit> = Self::opts(item, env)
            .into_iter()
            .enumerate()
            .map(|(at, o)| {
                let text = match o.digit {
                    Some(d) => format!("{d} {}", o.label),
                    None => o.label.clone(),
                };
                let suggested = card.and_then(|c| c.body.pick.as_ref()).is_some_and(
                    |p| matches!(&o.act, Act::Pick(Pick::Card { option, .. }) if *option == p.key),
                );
                Unit {
                    role: Role::Detail,
                    bold: on && suggested,
                    text: if suggested {
                        format!("{text} (qass suggests)")
                    } else {
                        text
                    },
                    hit: Some(Hit::Opt(slot.clone(), at)),
                }
            })
            .collect();
        if item.hand_to_qass {
            units.push(unit(
                Role::Detail,
                "h hand to qass",
                Some(Hit::Hand(slot.clone())),
            ));
        }
        if Self::takes_words(item, env) {
            units.push(unit(
                Role::Detail,
                "r reply in your own words",
                Some(Hit::Reply(slot.clone())),
            ));
        }
        let mut rows = if units.is_empty() {
            Vec::new()
        } else {
            flow(units, BODY, 3, cols)
        };
        if let Some(words) = self.drafts.get(slot).filter(|w| !w.is_empty()) {
            rows.extend(wrapped(
                &format!("draft: {words}"),
                BODY,
                cols,
                Role::FaintText,
                false,
            ));
        }
        rows
    }

    /// A held answer, with its target and its way out; nothing when none is held.
    fn hold_rows(&self, slot: &Slot, env: &Env, cols: usize) -> Vec<Row> {
        let Some(hold) = self.holds.of(slot) else {
            return Vec::new();
        };
        let left = hold.until.saturating_duration_since(env.now);
        let secs = left.as_secs_f64().ceil().max(1.0) as u64;
        flow(
            vec![
                Unit {
                    role: Role::Blocked,
                    bold: true,
                    text: hold_words(hold, secs, &env.who_slot(slot)),
                    hit: None,
                },
                unit(Role::Text, "u undo", Some(Hit::Undo(slot.clone()))),
                unit(
                    Role::Text,
                    "enter send now",
                    Some(Hit::SendNow(slot.clone())),
                ),
            ],
            BODY,
            3,
            cols,
        )
    }

    /// The selected card read whole: everything the daemon sent about it.
    fn whole_rows(&self, item: Option<&Item>, env: &Env, cols: usize) -> Vec<Row> {
        let Some(item) = item else {
            return vec![Row::plain(Role::FaintText, " nothing selected")];
        };
        let row = item.terminal.and_then(|t| env.row(t));
        let card = match &item.slot {
            Slot::Card(id) => env.card(id),
            Slot::Agent(_) => None,
        };
        let mut rows = wrapped(&env.who(item), 1, cols, Role::Text, true);
        // The answer keys stay, so a held answer is shown here too, with its way out.
        rows.extend(self.hold_rows(&item.slot, env, cols));
        let block = |label: &str, text: &str, rows: &mut Vec<Row>| {
            if text.is_empty() {
                return;
            }
            rows.push(Row::blank());
            rows.push(Row::plain(Role::FaintText, format!(" {label}")));
            rows.extend(wrapped(text, 3, cols, Role::Text, false));
        };
        if let Some(menu) = row.and_then(|r| r.menu.as_ref()) {
            block("it asks", &menu.question, &mut rows);
            for line in menu
                .context
                .iter()
                .filter(|l| l.trim() != menu.question.trim())
            {
                block("where", line, &mut rows);
            }
            for o in &menu.options {
                let detail = o.detail.clone().unwrap_or_default();
                block(
                    &format!("option {}", o.key),
                    &format!("{} {detail}", o.label),
                    &mut rows,
                );
            }
        }
        if let Some(card) = card {
            if let Some(asked) = &card.body.asked {
                block("you asked", asked, &mut rows);
            }
            block("it was doing", &card.body.doing, &mut rows);
            block("why it needs you", &card.body.why, &mut rows);
            block("the question", &card.body.asks, &mut rows);
            for (k, o) in card.body.options.iter().enumerate() {
                block(
                    &format!("answer {}: {}", k + 1, o.label),
                    &format!("it sends: {}", o.words),
                    &mut rows,
                );
                rows.extend(wrapped(
                    &format!("why: {}", o.reason),
                    3,
                    cols,
                    Role::FaintText,
                    false,
                ));
            }
            if let Some(pick) = &card.body.pick {
                block(
                    "qass would take",
                    &format!("{}: {}", pick.key, pick.reason),
                    &mut rows,
                );
            }
        }
        rows
    }
}

fn rule_row(cols: usize, more: &str) -> Row {
    if more.is_empty() || width::width(more) + 4 > cols {
        return Row::plain(Role::Rule, "\u{2500}".repeat(cols));
    }
    let tail = format!(" {more} ");
    Row {
        segs: vec![
            seg(
                Role::Rule,
                "\u{2500}".repeat(cols - width::width(&tail) - 1),
            ),
            seg(Role::FaintText, tail),
            seg(Role::Rule, "\u{2500}"),
        ],
        hits: Vec::new(),
    }
}

/// The hit under a one based screen position, for a click: `row` and `col` are the pane's own.
pub fn hit_at(view: &View, row: usize, col: usize) -> Option<Hit> {
    view.targets
        .iter()
        .find(|t| t.row == row && col >= t.x0 && col < t.x1)
        .map(|t| t.hit.clone())
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::answers::{ARMING, note_qass};

    #[derive(Deserialize)]
    struct State {
        now: i64,
        rows: Vec<AgentRow>,
        cards: Vec<AskInfo>,
        hand: HandInput,
        context: Context,
    }

    #[derive(Deserialize)]
    struct Fixture {
        states: BTreeMap<String, State>,
    }

    /// What the client holds for one state of the golden fixture (the design's records).
    struct World {
        rows: Vec<AgentRow>,
        spaces: Vec<WorkspaceRow>,
        cards: Vec<AskInfo>,
        hand: HandInput,
        ctx: Context,
        now_unix: i64,
        typed: BTreeMap<u32, Instant>,
    }

    fn space(id: i64, name: String) -> WorkspaceRow {
        WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id,
            name,
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            link_reason: None,
            goal: None,
        }
    }

    fn world(name: &str) -> World {
        let mut fixture: Fixture =
            serde_json::from_str(include_str!("../tests/fixtures/inbox-v3.json")).expect("fixture");
        let state = fixture.states.remove(name).expect("state");
        let mut hand = state.hand;
        hand.set_carded(&state.cards);
        note_qass(&mut hand, &state.rows, state.now);
        let mut spaces = Vec::new();
        for row in &state.rows {
            if spaces.iter().any(|s: &WorkspaceRow| s.id == row.workspace) {
                continue;
            }
            let name = match machine_of(row.workspace) {
                0 => format!("space{}", row.workspace),
                m => format!("{}:work", hand.machines[&m]),
            };
            spaces.push(space(row.workspace, name));
        }
        World {
            rows: state.rows,
            spaces,
            cards: state.cards,
            hand,
            ctx: state.context,
            now_unix: state.now,
            typed: BTreeMap::new(),
        }
    }

    fn env(w: &World, now: Instant) -> Env<'_> {
        Env {
            rows: &w.rows,
            spaces: &w.spaces,
            cards: &w.cards,
            hand: &w.hand,
            ctx: &w.ctx,
            now,
            now_unix: w.now_unix,
            typed: &w.typed,
        }
    }

    const PREFIX: u8 = 1;

    /// A pane whose menus have been seen at `t0`, and the time a digit is allowed.
    fn armed(w: &World, t0: Instant) -> (Pane, Instant) {
        let mut pane = Pane::new();
        pane.open();
        pane.rows_changed(&env(w, t0));
        (pane, t0 + ARMING + Duration::from_millis(100))
    }

    fn press(pane: &mut Pane, w: &World, at: Instant, keys: &[u8]) -> Out {
        pane.key(keys, PREFIX, &env(w, at))
    }

    fn menu_slot() -> Slot {
        Slot::Agent(2)
    }

    #[test]
    fn a_held_menu_answer_reaches_the_daemon_after_three_seconds() {
        let w = world("normal");
        let t0 = Instant::now();
        let (mut pane, ready) = armed(&w, t0);
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        let hold = pane.holds.of(&menu_slot()).expect("held").clone();
        assert_eq!(hold.until, ready + HOLD);
        // Not a moment early.
        pane.tick(&env(&w, ready + HOLD - Duration::from_millis(100)));
        assert!(pane.take().is_empty(), "sent before its time");
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        assert!(
            matches!(
                &sends.requests[..],
                [ControlRequest::AnswerMenu { terminal: 2, seal, .. }] if *seal == hold.seal
            ),
            "{sends:?}"
        );
        assert!(pane.holds.held.is_empty());
    }

    #[test]
    fn an_undone_answer_never_reaches_the_daemon() {
        let w = world("normal");
        let t0 = Instant::now();
        let (mut pane, ready) = armed(&w, t0);
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        assert!(pane.holds.of(&menu_slot()).is_some());
        press(&mut pane, &w, ready + Duration::from_secs(1), b"u");
        assert!(pane.holds.held.is_empty());
        pane.tick(&env(&w, ready + Duration::from_secs(10)));
        assert!(pane.take().is_empty(), "an undone answer was sent");
    }

    #[test]
    fn enter_sends_a_held_answer_now() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        press(&mut pane, &w, ready + Duration::from_millis(200), b"\r");
        assert!(pane.holds.held.is_empty());
        assert!(
            matches!(
                &pane.take().requests[..],
                [ControlRequest::AnswerMenu { terminal: 2, .. }]
            ),
            "send now did not send"
        );
    }

    #[test]
    fn a_digit_in_the_first_half_second_of_a_menu_goes_nowhere() {
        let w = world("normal");
        let t0 = Instant::now();
        let mut pane = Pane::new();
        pane.open();
        pane.rows_changed(&env(&w, t0));
        pane.select(menu_slot());
        press(&mut pane, &w, t0 + Duration::from_millis(100), b"1");
        assert!(
            pane.holds.held.is_empty(),
            "a key meant for the screen before it"
        );
    }

    #[test]
    fn a_changed_seal_drops_the_held_answer_and_says_who_answered() {
        let mut w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        assert!(pane.holds.of(&menu_slot()).is_some());
        // The agent put up another menu, or was answered somewhere else.
        let row = w.rows.iter_mut().find(|r| r.terminal == 2).expect("row");
        row.menu.as_mut().expect("menu").seal += 1;
        pane.rows_changed(&env(&w, ready + Duration::from_secs(1)));
        assert!(
            pane.holds.held.is_empty(),
            "kept an answer for another menu"
        );
        assert!(matches!(
            pane.said.get(&menu_slot()),
            Some(words) if words.contains("already answered")
        ));
        pane.tick(&env(&w, ready + Duration::from_secs(10)));
        assert!(pane.take().is_empty());
    }

    /// Claude asks the same question again: the text's seal is the same, the turn's is not. The
    /// answer held for the first goes to nobody, and a new process is another agent.
    #[test]
    fn the_same_question_asked_again_never_takes_the_held_answer() {
        for change in ["a new turn of the menu", "a new process"] {
            let mut w = world("normal");
            let row = w.rows.iter_mut().find(|r| r.terminal == 2).expect("row");
            (row.menu_seal, row.generation) = (Some(10), Some(1));
            let (mut pane, ready) = armed(&w, Instant::now());
            pane.select(menu_slot());
            press(&mut pane, &w, ready, b"1");
            let held = pane.holds.of(&menu_slot()).expect("held");
            assert_eq!((held.menu_seal, held.generation), (Some(10), Some(1)));
            let row = w.rows.iter_mut().find(|r| r.terminal == 2).expect("row");
            if change == "a new process" {
                row.generation = Some(2);
            } else {
                row.menu_seal = Some(11);
            }
            assert_eq!(
                row.menu.as_ref().expect("menu").seal,
                held.seal,
                "the same text"
            );
            pane.rows_changed(&env(&w, ready + Duration::from_secs(1)));
            assert!(pane.holds.held.is_empty(), "{change}: kept the old answer");
            pane.tick(&env(&w, ready + Duration::from_secs(10)));
            assert!(pane.take().is_empty(), "{change}: an answer was sent");
        }
    }

    /// The same question comes up again while a digit is typed: the first half second takes no
    /// digit, whatever the text's seal says; and a hand-off held on the old one goes nowhere.
    #[test]
    fn a_digit_right_after_the_same_question_comes_up_again_goes_nowhere() {
        let mut w = world("normal");
        let row = w.rows.iter_mut().find(|r| r.terminal == 2).expect("row");
        (row.menu_seal, row.generation) = (Some(10), Some(1));
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"h");
        assert!(
            pane.holds.of(&menu_slot()).is_some(),
            "the hand-off is held"
        );
        let again = ready + Duration::from_secs(1);
        w.rows
            .iter_mut()
            .find(|r| r.terminal == 2)
            .expect("row")
            .menu_seal = Some(11);
        pane.rows_changed(&env(&w, again));
        assert!(
            pane.holds.held.is_empty(),
            "the hand-off was for the old menu"
        );
        pane.tick(&env(&w, again + HOLD));
        assert!(pane.take().is_empty(), "a hand-off was sent");
        press(&mut pane, &w, again + Duration::from_millis(100), b"1");
        assert!(
            pane.holds.held.is_empty(),
            "a key meant for the screen before it"
        );
        press(&mut pane, &w, again + Duration::from_millis(600), b"1");
        assert!(pane.holds.of(&menu_slot()).is_some(), "armed again");
    }

    #[test]
    fn hand_to_qass_is_held_like_any_answer_and_goes_as_a_hand() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"h");
        assert_eq!(
            pane.holds.of(&menu_slot()).map(|h| &h.pick),
            Some(&Pick::Qass)
        );
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        assert!(
            matches!(
                &sends.requests[..],
                [ControlRequest::HandToQass { hands, .. }] if hands.len() == 1 && hands[0].terminal == 2
            ),
            "{sends:?}"
        );
        // `q` is the same key.
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"q");
        assert!(pane.holds.of(&menu_slot()).is_some());
    }

    /// The hand-off says what the phone says: "handing to qass: agent" while it waits, "handed
    /// to qass, 16:04: agent" once it went, and the offer is "Hand to qass".
    #[test]
    fn a_hand_off_says_what_the_phone_says() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        let menu = w
            .rows
            .iter()
            .find(|r| r.terminal == 2)
            .and_then(|r| r.menu.as_ref())
            .expect("menu");
        let labels: Vec<String> = crate::answers::offers(menu)
            .into_iter()
            .map(|o| o.label)
            .collect();
        assert_eq!(
            labels.last().map(String::as_str),
            Some("Hand to qass"),
            "{labels:?}"
        );
        press(&mut pane, &w, ready, b"h");
        let shown = pane.view(&env(&w, ready), 80, 40).plain.join("\n");
        assert!(shown.contains("handing to qass: "), "{shown}");
        assert!(shown.contains(", sending in 3 s"), "{shown}");
        let line = pane.held_line(&env(&w, ready)).expect("a held line");
        assert!(line.starts_with("handing to qass: "), "{line}");
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        let [ControlRequest::HandToQass { id, .. }] = &sends.requests[..] else {
            panic!("{sends:?}");
        };
        pane.answered(
            &ControlResponse::Handed {
                id: id.clone(),
                taken: vec![2],
            },
            &env(&w, ready + HOLD),
        );
        let said = pane
            .notice
            .as_ref()
            .map(|(s, _)| s.clone())
            .expect("a notice");
        let rest = said
            .strip_prefix("handed to qass, ")
            .unwrap_or_else(|| panic!("{said}"));
        let clock = rest.get(..5).unwrap_or(rest);
        assert!(rest[5..].starts_with(": "), "who follows the time: {said}");
        let ok = clock.as_bytes()[2] == b':'
            && clock.chars().filter(|c| c.is_ascii_digit()).count() == 4;
        assert!(ok, "{said}");
    }

    #[test]
    fn hand_to_qass_is_absent_while_qass_is_stopped() {
        let w = world("qass_stopped");
        let (mut pane, ready) = armed(&w, Instant::now());
        let inbox = pane.inbox(&env(&w, ready));
        assert!(
            inbox.items.iter().all(|i| !i.hand_to_qass),
            "an item offers the hand while qass is stopped"
        );
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"h");
        assert!(
            pane.holds.held.is_empty(),
            "handed to a qass that is not watching"
        );
        assert!(
            pane.notice
                .as_ref()
                .is_some_and(|(said, _)| said.contains("qass is not watching")),
            "{:?}",
            pane.notice
        );
        let view = pane.view(&env(&w, ready), 100, 400);
        let text = view.plain.join("\n");
        assert!(!text.contains("hand to qass"), "{text}");
    }

    #[test]
    fn hand_to_qass_is_absent_on_a_card_qass_asked() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        // E13 is qass's own question; Q15 an agent's.
        pane.select(Slot::Card("E13".into()));
        press(&mut pane, &w, ready, b"h");
        assert!(
            pane.holds.held.is_empty(),
            "qass was handed its own question"
        );
        assert!(
            pane.notice
                .as_ref()
                .is_some_and(|(said, _)| said.contains("qass asked this one")),
            "{:?}",
            pane.notice
        );
        let card = pane.card(
            &pane
                .inbox(&env(&w, ready))
                .items
                .into_iter()
                .find(|i| i.slot == Slot::Card("E13".into()))
                .expect("item"),
            true,
            &env(&w, ready),
            80,
        );
        let text: String = card
            .iter()
            .flat_map(|r| r.segs.iter().map(|s| s.text.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!text.contains("hand to qass"), "{text}");
        // An agent's card does take it.
        pane.select(Slot::Card("Q15".into()));
        press(&mut pane, &w, ready, b"h");
        assert_eq!(
            pane.holds.of(&Slot::Card("Q15".into())).map(|h| &h.pick),
            Some(&Pick::CardQass { card: "Q15".into() })
        );
    }

    #[test]
    fn a_card_option_is_held_and_written_with_the_words_the_person_saw() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Card("Q15".into()));
        press(&mut pane, &w, ready, b"2");
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        let [ControlRequest::AskWrite { action, .. }] = &sends.requests[..] else {
            panic!("{sends:?}");
        };
        let AskOp::Answer {
            card,
            option,
            seen,
            to_qass,
            ..
        } = action
        else {
            panic!("{action:?}");
        };
        assert_eq!(card, "Q15");
        assert_eq!(option.as_deref(), Some("b"));
        assert!(
            seen.as_ref().is_some_and(|s| !s.is_empty()),
            "no words seen"
        );
        assert!(!to_qass);
    }

    #[test]
    fn a_reply_is_held_then_typed_into_the_agent_and_dropped_when_it_moves_on() {
        let mut w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        // t:4 is an agent that ended its turn waiting for words.
        let slot = Slot::Agent(4);
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"r");
        assert!(pane.typing());
        press(&mut pane, &w, ready, b"use xheron");
        press(&mut pane, &w, ready, b"\r");
        assert!(matches!(
            pane.holds.of(&slot).map(|h| &h.pick),
            Some(Pick::Reply { text, .. }) if text == "use xheron"
        ));
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        assert!(sends.requests.is_empty(), "a reply is typed, not asked");
        assert_eq!(
            sends.typed,
            vec![
                Typed {
                    terminal: 4,
                    bytes: b"use xheron".to_vec(),
                    paste: true
                },
                Typed {
                    terminal: 4,
                    bytes: b"\r".to_vec(),
                    paste: false
                },
            ]
        );
        // Another turn: the same words are dropped.
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"use xheron\r");
        w.rows
            .iter_mut()
            .find(|r| r.terminal == 4)
            .expect("row")
            .stop = Some("another".into());
        pane.rows_changed(&env(&w, ready + Duration::from_secs(1)));
        assert!(pane.holds.held.is_empty(), "a reply kept for another turn");
        pane.tick(&env(&w, ready + Duration::from_secs(10)));
        assert!(pane.take().is_empty());
    }

    #[test]
    fn own_words_for_a_menu_go_as_a_typed_menu_answer() {
        let mut w = world("normal");
        // The agent's own field for an answer of one's own ("Type something.").
        w.rows
            .iter_mut()
            .find(|r| r.terminal == 2)
            .and_then(|r| r.menu.as_mut())
            .expect("menu")
            .options
            .push(
                serde_json::from_value(serde_json::json!({
                    "key": "9", "label": "Type something.", "input": true
                }))
                .expect("option"),
            );
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"not now\r");
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        assert!(
            matches!(&sends.requests[..], [ControlRequest::AnswerMenuText { text, .. }] if text == "not now"),
            "{sends:?}"
        );
    }

    /// Held for the menu of an agent on another machine (t:2197815297, "old-nas"), the answer
    /// as it was asked.
    fn asked_of_the_remote(w: &World) -> (Pane, Instant, String) {
        let (mut pane, ready) = armed(w, Instant::now());
        pane.select(Slot::Agent(2_197_815_297));
        press(&mut pane, w, ready, b"1");
        pane.tick(&env(w, ready + HOLD));
        let sends = pane.take();
        let [ControlRequest::AnswerMenu { id, .. }] = &sends.requests[..] else {
            panic!("{sends:?}");
        };
        (pane, ready + HOLD, id.clone())
    }

    /// Nothing is ever typed into a pane the person did not choose: an answer a remote refuses
    /// as unknown stays a refusal on its card, in either set of words.
    #[test]
    fn a_refused_answer_is_never_typed_into_a_pane() {
        let w = world("normal");
        let own = "this daemon is older than the client that called it: unknown variant \
                   `AnswerMenu`";
        let mux = format!("old-nas {}", crate::mux::OLDER_QOOP);
        for reason in [own.to_string(), mux] {
            let (mut pane, at, id) = asked_of_the_remote(&w);
            assert!(pane.answered(&ControlResponse::Failed { id, reason }, &env(&w, at)));
            let sends = pane.take();
            assert!(sends.typed.is_empty(), "typed into a pane: {sends:?}");
            assert!(sends.requests.is_empty());
            let said = pane.said.get(&Slot::Agent(2_197_815_297)).expect("a word");
            assert!(said.contains("too old"), "{said}");
        }
    }

    /// The refusal with no id (a daemon too old to read the request) belongs to the answer it
    /// names; the mux gives it back under the id.
    #[test]
    fn a_refusal_with_no_id_belongs_to_the_answer_it_names() {
        let w = world("normal");
        let (mut pane, at, _) = asked_of_the_remote(&w);
        let reason = "this daemon is older than the client that called it: unknown variant \
                      `AnswerMenu`"
            .to_string();
        assert!(pane.answered(
            &ControlResponse::Failed {
                id: String::new(),
                reason
            },
            &env(&w, at)
        ));
        assert!(pane.take().typed.is_empty());
        assert!(pane.said.contains_key(&Slot::Agent(2_197_815_297)));
    }

    /// An answer sent for a menu that has since gone is not said under what the agent asks now.
    #[test]
    fn an_answer_for_a_menu_that_is_gone_says_nothing() {
        let mut w = world("normal");
        let (mut pane, at, id) = asked_of_the_remote(&w);
        w.rows
            .iter_mut()
            .find(|r| r.terminal == 2_197_815_297)
            .expect("row")
            .menu = None;
        pane.answered(
            &ControlResponse::Failed {
                id,
                reason: "already_answered".into(),
            },
            &env(&w, at),
        );
        assert!(pane.said.is_empty(), "{:?}", pane.said);
    }

    #[test]
    fn a_sent_answer_is_said_and_kept_for_answered_today() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        let [ControlRequest::AnswerMenu { id, .. }] = &sends.requests[..] else {
            panic!("{sends:?}");
        };
        pane.answered(
            &ControlResponse::MenuAnswered {
                id: id.clone(),
                answers: true,
            },
            &env(&w, ready + HOLD),
        );
        assert!(
            pane.notice
                .as_ref()
                .is_some_and(|(s, _)| s.starts_with("sent to"))
        );
        let inbox = pane.inbox(&env(&w, ready + HOLD));
        assert!(inbox.answered.iter().any(|a| a.own), "{:?}", inbox.answered);
    }

    /// An answer has gone and the same question comes up again (same text seal, new turn): it
    /// takes a digit once its guard is up, and nothing said about the old answer stands under
    /// it, whether the daemon's answer came before the new menu or after it.
    #[test]
    fn the_same_question_after_a_sent_answer_is_answerable_with_no_stale_line() {
        for answer_late in [false, true] {
            let mut w = world("normal");
            let row = w.rows.iter_mut().find(|r| r.terminal == 2).expect("row");
            (row.menu_seal, row.generation) = (Some(10), Some(1));
            let (mut pane, ready) = armed(&w, Instant::now());
            pane.select(menu_slot());
            press(&mut pane, &w, ready, b"1");
            pane.tick(&env(&w, ready + HOLD));
            let sends = pane.take();
            let [ControlRequest::AnswerMenu { id, .. }] = &sends.requests[..] else {
                panic!("{sends:?}");
            };
            let answered = ControlResponse::MenuAnswered {
                id: id.clone(),
                answers: true,
            };
            if !answer_late {
                pane.answered(&answered, &env(&w, ready + HOLD));
                assert!(
                    pane.notice
                        .as_ref()
                        .is_some_and(|(s, _)| s.starts_with("sent to"))
                );
            }
            // The same question again: the same text, another turn.
            let again = ready + HOLD + Duration::from_secs(1);
            w.rows
                .iter_mut()
                .find(|r| r.terminal == 2)
                .expect("row")
                .menu_seal = Some(11);
            pane.rows_changed(&env(&w, again));
            if answer_late {
                pane.answered(&answered, &env(&w, again));
            }
            assert!(
                pane.notice.is_none() && !pane.said.contains_key(&menu_slot()),
                "late {answer_late}: a stale line: {:?} {:?}",
                pane.notice,
                pane.said
            );
            let shown = pane.view(&env(&w, again), 80, 40).plain.join("\n");
            assert!(!shown.contains("sent to"), "{shown}");
            press(&mut pane, &w, again + ARMING, b"1");
            assert!(
                pane.holds.of(&menu_slot()).is_some(),
                "late {answer_late}: the new question took no digit"
            );
        }
    }

    #[test]
    fn leaving_sends_what_is_on_hold() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        pane.flush(&env(&w, ready + Duration::from_millis(50)));
        assert!(matches!(
            &pane.take().requests[..],
            [ControlRequest::AnswerMenu { terminal: 2, .. }]
        ));
    }

    #[test]
    fn f_shows_only_what_qass_wrote_and_f_again_clears_it() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        let all = pane.order(&pane.inbox(&env(&w, ready)));
        press(&mut pane, &w, ready, b"f");
        assert!(pane.filter);
        let inbox = pane.inbox(&env(&w, ready));
        let filtered = pane.order(&inbox);
        assert!(!filtered.is_empty() && filtered.len() < all.len());
        for slot in &filtered {
            let item = inbox.items.iter().find(|i| i.slot == *slot).expect("item");
            assert!(item.kind.from_qass(), "{slot:?} is not from qass");
        }
        let view = pane.view(&env(&w, ready), 80, 400);
        let text = view.plain.join("\n");
        assert!(text.contains("from qass"), "{text}");
        assert!(
            !text.contains("with qass"),
            "the quiet sections stay out of the filter"
        );
        press(&mut pane, &w, ready, b"f");
        assert!(!pane.filter);
        assert_eq!(pane.order(&pane.inbox(&env(&w, ready))), all);
    }

    #[test]
    fn j_and_k_move_through_the_list_oldest_first() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        let order = pane.order(&pane.inbox(&env(&w, ready)));
        assert_eq!(pane.selected(&order), Some(order[0].clone()));
        press(&mut pane, &w, ready, b"j");
        press(&mut pane, &w, ready, b"j");
        assert_eq!(pane.selected(&order), Some(order[2].clone()));
        press(&mut pane, &w, ready, b"k");
        assert_eq!(pane.selected(&order), Some(order[1].clone()));
        press(&mut pane, &w, ready, b"\x1b[B");
        assert_eq!(pane.selected(&order), Some(order[2].clone()));
    }

    #[test]
    fn esc_goes_back_and_the_prefix_and_the_mouse_are_not_the_inboxs() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        assert_eq!(press(&mut pane, &w, ready, b"\x1b"), Out::Closed);
        assert_eq!(
            press(&mut pane, &w, ready, b"\x01x"),
            Out::Pass(b"\x01x".to_vec())
        );
        assert_eq!(
            press(&mut pane, &w, ready, b"\x11"),
            Out::Pass(b"\x11".to_vec())
        );
        // `n` and tab are kept for later: taken, never typed into the agent behind.
        assert_eq!(press(&mut pane, &w, ready, b"n"), Out::Stay);
        assert_eq!(press(&mut pane, &w, ready, b"\t"), Out::Stay);
    }

    #[test]
    fn a_failed_agent_offers_to_open_it_and_a_digit_goes_there() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Agent(9));
        assert_eq!(press(&mut pane, &w, ready, b"1"), Out::Jump(9));
        assert_eq!(press(&mut pane, &w, ready, b"g"), Out::Jump(9));
    }

    // The view ------------------------------------------------------------------------------

    /// The pane's size for a window of `cols` by `lines`, as the client works it out: the
    /// column folds to its strip below 100 columns, and the Inbox takes the tab bar's rows too.
    fn dims(cols: u16, lines: u16) -> (usize, usize) {
        let (c, r) = crate::screen::pane_size_for(
            cols,
            lines,
            cols < INBOX_FOLDS_BELOW,
            0,
            crate::sidebar::WIDTH,
        );
        (c as usize, r as usize + crate::screen::TABS)
    }

    fn flat(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Every word of every card is on screen, whole: nothing is cut to an ellipsis, however
    /// narrow, and an option never loses its number.
    fn nothing_cut(w: &World, cols: usize) {
        let (pane, _) = armed(w, Instant::now());
        let now = Instant::now();
        let view = pane.view(&env(w, now), cols, 2000);
        for row in &view.plain {
            assert!(
                !row.contains('\u{2026}'),
                "a row ends in an ellipsis: {row:?}"
            );
            assert!(width::width(row) <= cols, "a row is too wide: {row:?}");
        }
        let all = flat(&view.plain.join(" "));
        let inbox = pane.inbox(&env(w, now));
        for item in &inbox.items {
            let row = item
                .terminal
                .and_then(|t| w.rows.iter().find(|r| r.terminal == t));
            let said = match (&item.slot, row.and_then(|r| r.menu.as_ref())) {
                (_, Some(menu)) if item.kind == ItemKind::Menu => menu.question.clone(),
                (Slot::Card(id), _) => w
                    .cards
                    .iter()
                    .find(|c| c.card == *id)
                    .map(|c| c.body.asks.clone())
                    .unwrap_or_default(),
                _ => String::new(),
            };
            assert!(
                all.contains(&flat(&said)),
                "cut at {cols} columns: {said:?}"
            );
            for (k, opt) in Pane::opts(item, &env(w, now)).iter().enumerate() {
                let text = match opt.digit {
                    Some(d) => format!("{d} {}", opt.label),
                    None => opt.label.clone(),
                };
                assert!(
                    all.contains(&flat(&text)),
                    "option {k} of {:?} is cut at {cols} columns: {text:?}",
                    item.slot
                );
            }
        }
    }

    #[test]
    fn nothing_is_cut_at_80_104_and_220_columns() {
        let w = world("normal");
        for (cols, lines) in [(80u16, 24u16), (104, 30), (220, 60)] {
            nothing_cut(&w, dims(cols, lines).0);
        }
        // And narrower than any of them, where the words wrap most.
        nothing_cut(&w, 36);
    }

    #[test]
    fn the_selected_card_is_always_on_screen_at_80x24() {
        let w = world("normal");
        let (cols, rows) = dims(80, 24);
        let (mut pane, ready) = armed(&w, Instant::now());
        let order = pane.order(&pane.inbox(&env(&w, ready)));
        for slot in &order {
            pane.select(slot.clone());
            let view = pane.view(&env(&w, ready), cols, rows);
            assert_eq!(view.rows.len(), rows);
            let text = view.plain.join("\n");
            let marker = view.plain.iter().filter(|r| r.contains('\u{258c}')).count();
            assert!(marker >= 1, "no row marks {slot:?}:\n{text}");
        }
    }

    #[test]
    fn the_header_counts_by_the_one_rule_and_names_qass() {
        let w = world("normal");
        let (pane, ready) = armed(&w, Instant::now());
        let view = pane.view(&env(&w, ready), 100, 2000);
        let head = view.plain[0].clone();
        assert!(head.contains("inbox"), "{head:?}");
        assert!(head.contains("needs you 17"), "{head:?}");
        assert!(head.contains("qass watching"), "{head:?}");
        // The quiet sections are below the list, not in the count.
        let text = view.plain.join("\n");
        assert!(text.contains("with qass 2"), "{text}");
        assert!(text.contains("finished 2"), "{text}");
        // The four read from ended cards; this client has sent none of its own.
        assert!(text.contains("answered today 4"), "{text}");
        let stopped = world("qass_stopped");
        let (pane, ready) = armed(&stopped, Instant::now());
        let view = pane.view(&env(&stopped, ready), 100, 2000);
        assert!(
            view.plain[0].contains("qass not watching"),
            "{:?}",
            view.plain[0]
        );
        assert!(
            view.plain[0].contains("needs you 19"),
            "{:?}",
            view.plain[0]
        );
        let first = world("first_run");
        let (pane, ready) = armed(&first, Instant::now());
        let view = pane.view(&env(&first, ready), 100, 20);
        assert!(
            view.plain[0].contains("nothing needs you"),
            "{:?}",
            view.plain[0]
        );
        assert!(view.plain.join("\n").contains("nothing needs you"));
    }

    #[test]
    fn a_computer_not_reached_says_so_and_takes_no_answer() {
        let w = world("computer_lost");
        let (mut pane, ready) = armed(&w, Instant::now());
        let view = pane.view(&env(&w, ready), 100, 80);
        let text = view.plain.join("\n");
        assert!(text.contains("build-node is not reached"), "{text}");
        assert!(text.contains("waits until it is back"), "{text}");
        pane.select(Slot::Agent(2_164_260_865));
        press(&mut pane, &w, ready, b"1");
        assert!(
            pane.holds.held.is_empty(),
            "an answer was held for a computer not reached"
        );
    }

    #[test]
    fn a_hold_shows_its_countdown_its_target_and_its_way_out() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        let view = pane.view(&env(&w, ready), 80, 400);
        let text = view.plain.join("\n");
        assert!(text.contains("sending in 3 s:"), "{text}");
        assert!(
            text.contains("u undo") && text.contains("enter send now"),
            "{text}"
        );
        // Both are clickable.
        assert!(view.targets.iter().any(|t| matches!(t.hit, Hit::Undo(_))));
        assert!(
            view.targets
                .iter()
                .any(|t| matches!(t.hit, Hit::SendNow(_)))
        );
        let later = pane.view(&env(&w, ready + Duration::from_millis(1200)), 80, 400);
        assert!(later.plain.join("\n").contains("sending in 2 s:"));
        // The status row keeps the word while the Inbox is put away.
        assert!(
            pane.held_line(&env(&w, ready))
                .is_some_and(|l| l.contains("undoes") && l.contains(" to "))
        );
    }

    #[test]
    fn a_click_on_an_option_holds_it_and_a_click_on_undo_undoes_it() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        let view = pane.view(&env(&w, ready), 80, 400);
        let opt = view
            .targets
            .iter()
            .find(|t| matches!(&t.hit, Hit::Opt(s, 0) if *s == menu_slot()))
            .expect("the first answer is clickable");
        let hit = hit_at(&view, opt.row, opt.x0).expect("hit");
        pane.click(hit, &env(&w, ready));
        assert!(pane.holds.of(&menu_slot()).is_some());
        pane.click(Hit::Undo(menu_slot()), &env(&w, ready));
        assert!(pane.holds.held.is_empty());
    }

    #[test]
    fn the_caret_sits_at_the_end_of_the_typed_words() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        // A long target name, so the label above the box wraps onto two rows.
        pane.select(Slot::Agent(4));
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"use xheron");
        for cols in [75usize, 30] {
            let view = pane.view(&env(&w, ready), cols, 24);
            let (row, col) = view.cursor.expect("a caret while typing");
            assert!(
                view.plain[row].trim_end().ends_with("use xheron"),
                "{:?}",
                view.plain[row]
            );
            assert_eq!(col, width::width(view.plain[row].trim_end()), "at {cols}");
        }
    }

    #[test]
    fn m_reads_the_card_whole_and_esc_goes_back_to_the_list() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Card("Q15".into()));
        press(&mut pane, &w, ready, b"m");
        let view = pane.view(&env(&w, ready), 80, 100);
        let text = view.plain.join("\n");
        assert!(text.contains("why it needs you"), "{text}");
        assert!(
            text.contains("it sends:") && text.contains("why:"),
            "{text}"
        );
        assert_eq!(press(&mut pane, &w, ready, b"\x1b"), Out::Stay);
        assert!(!pane.whole);
    }

    /// The Inbox at the sizes the plan names, as the text a person sees: written to
    /// `QOOP_INBOX_SHOTS` when it is set, so a run can keep them.
    #[test]
    fn the_inbox_at_three_sizes_for_the_record() {
        let w = world("normal");
        let keep = |name: &str, view: &View| {
            if let Ok(dir) = std::env::var("QOOP_INBOX_SHOTS") {
                let path = std::path::Path::new(&dir).join(name);
                std::fs::write(path, format!("{}\n", view.plain.join("\n")))
                    .expect("write the capture");
            }
        };
        for (cols, lines) in [(80u16, 24u16), (104, 30), (220, 60)] {
            let (c, r) = dims(cols, lines);
            let (pane, ready) = armed(&w, Instant::now());
            let view = pane.view(&env(&w, ready), c, r);
            assert_eq!(view.plain.len(), r);
            keep(&format!("inbox-{cols}x{lines}.txt"), &view);
            assert!(view.plain.join("\n").contains("needs you 17"));
        }
        // The other states the plan names, at 80x24: a hold, the person's own words, a card read
        // whole, qass stopped, a computer not reached, nothing at all.
        let (c, r) = dims(80, 24);
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"1");
        keep("inbox-80x24-hold.txt", &pane.view(&env(&w, ready), c, r));
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Agent(4));
        press(&mut pane, &w, ready, b"r");
        press(
            &mut pane,
            &w,
            ready,
            b"The org is xheron, and the repos are private",
        );
        keep("inbox-80x24-typing.txt", &pane.view(&env(&w, ready), c, r));
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Card("Q15".into()));
        press(&mut pane, &w, ready, b"m");
        keep("inbox-80x24-whole.txt", &pane.view(&env(&w, ready), c, r));
        for (state, name) in [
            ("qass_stopped", "stopped"),
            ("computer_lost", "lost"),
            ("first_run", "empty"),
        ] {
            let w = world(state);
            let (pane, ready) = armed(&w, Instant::now());
            keep(
                &format!("inbox-80x24-{name}.txt"),
                &pane.view(&env(&w, ready), c, r),
            );
        }
        let (mut pane, ready) = armed(&w, Instant::now());
        press(&mut pane, &w, ready, b"f");
        keep(
            "inbox-80x24-from-qass.txt",
            &pane.view(&env(&w, ready), c, r),
        );
    }

    #[test]
    fn typed_words_survive_leaving_the_inbox_and_moving_to_another_card() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        let slot = Slot::Agent(4);
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"use xheron");
        // `ctrl-a i` or `ctrl-a tab` away: the Inbox is put away.
        pane.close();
        assert!(!pane.typing());
        pane.open();
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"r");
        assert_eq!(pane.text, "use xheron", "the words were lost on close");
        // A click on another card.
        let other = Slot::Card("Q15".into());
        pane.click(Hit::Item(other.clone()), &env(&w, ready));
        assert!(!pane.typing());
        let shown = pane.view(&env(&w, ready), 80, 400).plain.join("\n");
        assert!(shown.contains("draft: use xheron"), "{shown}");
        pane.click(Hit::Item(slot), &env(&w, ready));
        press(&mut pane, &w, ready, b"r");
        assert_eq!(pane.text, "use xheron", "the words were lost on a click");
    }

    #[test]
    fn a_held_reply_is_not_typed_into_a_line_the_person_began() {
        let mut w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        let slot = Slot::Agent(4);
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"use xheron\r");
        assert!(pane.holds.of(&slot).is_some());
        // The person typed into that terminal after the reply was held.
        w.typed.insert(4, ready + Duration::from_secs(1));
        pane.tick(&env(&w, ready + HOLD));
        let sends = pane.take();
        assert!(sends.typed.is_empty(), "typed into a line the person began");
        assert!(pane.holds.held.is_empty());
        let shown = pane.view(&env(&w, ready + HOLD), 80, 400).plain.join("\n");
        assert!(
            shown.contains("you typed in that terminal; your reply is kept here"),
            "{shown}"
        );
        assert!(shown.contains("draft: use xheron"), "{shown}");
        // Typing there before the hold began changes nothing.
        let (mut pane, ready) = armed(&w, Instant::now() + Duration::from_secs(5));
        w.typed.insert(4, ready - Duration::from_secs(4));
        pane.select(slot);
        press(&mut pane, &w, ready, b"r");
        press(&mut pane, &w, ready, b"use xheron\r");
        pane.tick(&env(&w, ready + HOLD));
        assert_eq!(pane.take().typed.len(), 2);
    }

    #[test]
    fn put_away_is_held_like_an_answer_and_can_be_undone() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        let slot = Slot::Card("Q15".into());
        pane.select(slot.clone());
        press(&mut pane, &w, ready, b"x");
        assert!(pane.take().is_empty(), "put away at once, no way back");
        assert_eq!(
            pane.holds.of(&slot).map(|h| h.label.as_str()),
            Some("Put away")
        );
        press(&mut pane, &w, ready, b"u");
        pane.tick(&env(&w, ready + HOLD));
        assert!(pane.take().is_empty(), "an undone put away was sent");
        press(&mut pane, &w, ready, b"x");
        press(&mut pane, &w, ready, b"\r");
        assert!(matches!(
            &pane.take().requests[..],
            [ControlRequest::AskWrite {
                action: AskOp::Dismiss { .. },
                ..
            }]
        ));
        // A menu is not a card.
        pane.select(menu_slot());
        press(&mut pane, &w, ready, b"x");
        assert!(pane.holds.of(&menu_slot()).is_none());
    }

    #[test]
    fn reading_a_card_whole_keeps_the_hold_bar_and_the_answer_keys() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        pane.select(Slot::Card("Q15".into()));
        press(&mut pane, &w, ready, b"m");
        press(&mut pane, &w, ready, b"1");
        assert!(pane.holds.of(&Slot::Card("Q15".into())).is_some());
        let shown = pane.view(&env(&w, ready), 80, 100).plain.join("\n");
        assert!(shown.contains("sending in 3 s:"), "{shown}");
        assert!(
            shown.contains("u undo") && shown.contains("enter send now"),
            "{shown}"
        );
        press(&mut pane, &w, ready, b"u");
        assert!(pane.holds.held.is_empty());
    }

    #[test]
    fn an_agent_with_a_menu_and_a_card_is_one_terminal_in_the_palettes_list() {
        let w = world("normal");
        let (pane, ready) = armed(&w, Instant::now());
        let inbox = pane.inbox(&env(&w, ready));
        let all: Vec<u32> = inbox.items.iter().filter_map(|i| i.terminal).collect();
        let once = inbox.terminals();
        assert!(
            all.len() > once.len(),
            "the fixture has no agent with two items"
        );
        let mut sorted = once.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), once.len(), "{once:?}");
    }

    #[test]
    fn the_start_of_the_day_is_looked_up_only_when_the_day_may_have_changed() {
        let mut cache = 0;
        let now = 1_795_400_000;
        let start = day_start(&mut cache, now);
        assert!(start <= now && now - start < 25 * 3600, "{start}");
        // Inside the day: the kept value, whatever is in the cache.
        let mut kept = now - 3600;
        assert_eq!(day_start(&mut kept, now), now - 3600);
        // A day on, or the clock set back: looked up again.
        let mut old = now - 24 * 3600;
        assert_eq!(day_start(&mut old, now), start);
        let mut ahead = now + 3600;
        assert_eq!(day_start(&mut ahead, now), start);
    }

    #[test]
    fn a_prefix_in_the_middle_of_a_read_is_passed_on_not_swallowed() {
        let w = world("normal");
        let (mut pane, ready) = armed(&w, Instant::now());
        // `j` is the Inbox's, the prefix and what follows are not.
        assert_eq!(
            press(&mut pane, &w, ready, b"j\x01i"),
            Out::Pass(b"\x01i".to_vec())
        );
        let order = pane.order(&pane.inbox(&env(&w, ready)));
        assert_eq!(pane.selected(&order), Some(order[1].clone()));
        // And while typing: the words before it are kept.
        pane.select(Slot::Agent(4));
        press(&mut pane, &w, ready, b"r");
        assert_eq!(
            press(&mut pane, &w, ready, b"ab\x01i"),
            Out::Pass(b"\x01i".to_vec())
        );
        assert_eq!(pane.text, "ab");
    }
}
