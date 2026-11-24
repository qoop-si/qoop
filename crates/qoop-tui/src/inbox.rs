//! The Inbox: everything that needs the person, as one list, built from what the daemon said.
//!
//! Agent rows (`AgentRow`) and question cards (`AskInfo`) go in; one oldest-first list of
//! items, one count and the quiet sections come out. Plain values and functions: no screen, no
//! clock, no daemon. Every surface (the terminal interface, the desktop, and the phone through
//! the shared golden fixture `tests/fixtures/inbox-v3.json`) reads the same rules from here.
//!
//! The count is the length of the list; a question that waits on qass (a hand to it) and
//! finished work nobody has read are not in it, they have their sections.

use std::collections::BTreeSet;

use qoop_domain::status::Status;
use qoop_runtime::control::{AgentRow, AskInfo};
use serde::Deserialize;

use crate::answers::{HandInput, Kind, Slot, can_hand, handed};
use crate::asks::plan_card;
use crate::mux::machine_of;

/// What an item is, and so what it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// An agent's own menu on its screen: its options, Explain, Hand to qass, own words.
    Menu,
    /// An agent that ended its turn waiting for the person's words (its task waits on a human
    /// and no menu is up): a field to type in.
    Reply,
    /// A dialog qoop cannot read, on an agent's screen (a blocked row with no menu) or as a
    /// draft card the daemon filed for it: Open the agent. No Hand to qass (the daemon routes
    /// a dialog's answer to qass itself).
    Dialog,
    /// A blocked agent from a machine whose qoop sends no menu: Open the agent, whatever it
    /// asks. Never a dialog card, which would say more than is known.
    Open,
    /// A card from qass: one it asked (`E`), or a notice about qass itself (a `W` card with no
    /// agent). No Hand to qass.
    Qass,
    /// A card an agent asked (`Q`, `qoop task ask`) or the daemon's draft for its stop (`W` with
    /// a task).
    Agent,
    /// A card asking to approve an item's plan.
    Plan,
    /// An agent that failed: Open the agent.
    Failed,
}

impl ItemKind {
    /// The word the golden fixture and the phone use for it.
    pub fn token(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Reply => "reply",
            Self::Dialog => "dialog",
            Self::Open => "open",
            Self::Qass => "qass",
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Failed => "failed",
        }
    }

    /// Whether qass wrote it (a qass card or a plan): the "From qass" filter.
    pub fn from_qass(self) -> bool {
        matches!(self, Self::Qass | Self::Plan)
    }
}

/// One thing that needs the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The agent's terminal for a row, the card's id for a card: also what a hold is keyed by.
    pub slot: Slot,
    pub kind: ItemKind,
    /// The agent it is about, when there is one: a row's own, or the one a card names.
    pub terminal: Option<u32>,
    /// Since when it waits: the row's `last_active_at_unix`, a card's `opened_at`. `None` for a
    /// row the daemon has not stamped (after a restart), which lists first rather than fresh.
    /// Each is on the clock of the daemon that wrote it (a row's on its machine's, a card's on
    /// This Mac's), so two machines' clocks can shift the order a little.
    pub since: Option<i64>,
    /// Whether "Hand to qass" is offered: only on an agent's menu and on a card an agent
    /// asked, only while qass runs and watches, never on a machine not reached.
    pub hand_to_qass: bool,
    /// The machine it is on is not reached: what shows is as of its last answer, and
    /// answering waits until the machine is back.
    pub lost: bool,
}

impl Item {
    /// `t:<terminal>` or `c:<card>`: the key the fixture orders by.
    pub fn key(&self) -> String {
        match &self.slot {
            Slot::Agent(t) => format!("t:{t}"),
            Slot::Card(c) => format!("c:{c}"),
        }
    }
}

/// What the window knows around the rows that the rows do not say.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Context {
    /// When the person's local day began, unix seconds: "today" for Answered today. The caller
    /// refreshes it when midnight passes.
    pub day_start: i64,
    /// Machines not reached now (their rows are the last ones read).
    pub lost: BTreeSet<usize>,
    /// Machines whose qoop is too old to send a menu with a blocked agent.
    pub old: BTreeSet<usize>,
    /// The spaces qass lives in on This Mac: its own tab is never the person's to answer.
    pub qass_spaces: BTreeSet<i64>,
}

/// An answer this client sent itself, for Answered today: agent menus leave no card, so only
/// the client that sent one can say it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct OwnAnswer {
    pub at: i64,
    pub label: String,
    /// Whom it was for: the agent's name.
    pub to: String,
    /// The card it answered, when it was a card: the ended card is then not listed twice.
    #[serde(default)]
    pub card: Option<String>,
}

/// One line of Answered today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    pub at: i64,
    pub label: String,
    pub to: String,
    /// Sent by this client, rather than read from an ended card.
    pub own: bool,
}

/// The Inbox.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inbox {
    /// What needs the person, oldest first.
    pub items: Vec<Item>,
    /// The one count: `items.len()`. Handed questions and finished work are not in it.
    pub count: usize,
    /// With qass: the terminals of questions qass holds now, oldest first.
    pub with_qass: Vec<u32>,
    /// Finished: the terminals of agents done and unread, oldest first.
    pub finished: Vec<u32>,
    /// Answered today, newest first: ended cards closed today, then this client's own answers.
    pub answered: Vec<Answered>,
}

/// Where a row goes.
enum Place {
    Item(Item),
    WithQass,
    Finished,
    Nowhere,
}

impl Inbox {
    /// Build the list. `rows` are the agents of every machine, `cards` every card read with
    /// `AskRead { all: true }` (open, answered and ended alike); `hand` is what [`handed`]
    /// needs, with `carded` set from the same cards ([`HandInput::set_carded`]). Rows in qass's
    /// own space are dropped.
    ///
    /// One item per open loop. A menu and a failure stay their agent's own items whatever
    /// cards there are (the live menu answers through its own path); an open card about an
    /// agent that waits for words or on a dialog (Reply, Dialog, Open) replaces that agent's
    /// row, so the question is not listed twice. Oldest first, ties by agent before card and
    /// then by slot.
    pub fn build(
        rows: &[AgentRow],
        cards: &[AskInfo],
        hand: &HandInput,
        ctx: &Context,
        own: &[OwnAnswer],
    ) -> Inbox {
        let rows: Vec<&AgentRow> = rows
            .iter()
            .filter(|r| r.agent && !ctx.qass_spaces.contains(&r.workspace))
            .collect();
        let open: Vec<(&AskInfo, Option<&AgentRow>)> = cards
            .iter()
            .filter(|c| c.status == "open")
            .map(|c| (c, card_row(c, &rows, hand)))
            .collect();
        let carded: BTreeSet<u32> = open
            .iter()
            .filter_map(|(_, row)| row.map(|r| r.terminal))
            .collect();

        let mut items: Vec<Item> = Vec::new();
        let (mut with_qass, mut finished) = (Vec::new(), Vec::new());
        for row in &rows {
            match row_item(row, hand, ctx, &carded) {
                Place::Item(item) => items.push(item),
                Place::WithQass => with_qass.push(*row),
                Place::Finished => finished.push(*row),
                Place::Nowhere => {}
            }
        }
        items.extend(
            open.iter()
                .map(|(card, row)| card_item(card, *row, hand, ctx)),
        );
        // Oldest first; an unstamped one first, then by slot so the order never wobbles.
        items.sort_by_key(|i| (i.since.unwrap_or(i64::MIN), i.slot.clone()));
        Inbox {
            count: items.len(),
            items,
            with_qass: oldest(with_qass),
            finished: oldest(finished),
            answered: answered_today(cards, own, ctx.day_start),
        }
    }
}

/// The terminals of `rows`, oldest first.
fn oldest(mut rows: Vec<&AgentRow>) -> Vec<u32> {
    rows.sort_by_key(|r| (r.last_active_at_unix.unwrap_or(i64::MIN), r.terminal));
    rows.iter().map(|r| r.terminal).collect()
}

/// Where one agent row goes: its own item, With qass, Finished, or nowhere. `carded` holds the
/// agents an open card is about.
fn row_item(row: &AgentRow, hand: &HandInput, ctx: &Context, carded: &BTreeSet<u32>) -> Place {
    let lost = ctx.lost.contains(&machine_of(row.workspace));
    let kind = match Kind::of(row) {
        Kind::Sleeping => return Place::Nowhere,
        Kind::Status(Status::Done) => return Place::Finished,
        Kind::Status(Status::Blocked) if handed(row, hand) => return Place::WithQass,
        kind => match row_kind(row, kind, ctx) {
            Some(kind) => kind,
            None => return Place::Nowhere,
        },
    };
    let standing_in = matches!(kind, ItemKind::Reply | ItemKind::Dialog | ItemKind::Open);
    if standing_in && carded.contains(&row.terminal) {
        return Place::Nowhere;
    }
    Place::Item(Item {
        slot: Slot::Agent(row.terminal),
        kind,
        terminal: Some(row.terminal),
        since: row.last_active_at_unix,
        hand_to_qass: kind == ItemKind::Menu
            && !lost
            && can_hand(row, hand)
            && row.menu.as_ref().is_some_and(|m| !m.multi),
        lost,
    })
}

/// The item of an open card, `row` the agent it is about when this client has its row.
fn card_item(card: &AskInfo, row: Option<&AgentRow>, hand: &HandInput, ctx: &Context) -> Item {
    let kind = card_kind(card);
    let lost = row.is_some_and(|r| ctx.lost.contains(&machine_of(r.workspace)));
    let can = row.map_or_else(|| hand.qass_off().is_none(), |r| can_hand(r, hand));
    Item {
        slot: Slot::Card(card.card.clone()),
        kind,
        terminal: row.map(|r| r.terminal).or(card_terminal(card)),
        since: Some(card.opened_at),
        hand_to_qass: kind == ItemKind::Agent && !lost && can,
        lost,
    }
}

/// What a row asks of the person, from its status (the daemon's, never worked out here), or
/// `None` when it asks nothing.
fn row_kind(row: &AgentRow, kind: Kind, ctx: &Context) -> Option<ItemKind> {
    match kind {
        Kind::Status(Status::Failed) => Some(ItemKind::Failed),
        Kind::Status(Status::Blocked) if row.menu.is_some() => Some(ItemKind::Menu),
        // Blocked by its task or a card while no dialog is up: it waits for words.
        Kind::Status(Status::Blocked) if row.state != "blocked" => Some(ItemKind::Reply),
        Kind::Status(Status::Blocked) if ctx.old.contains(&machine_of(row.workspace)) => {
            Some(ItemKind::Open)
        }
        Kind::Status(Status::Blocked) => Some(ItemKind::Dialog),
        _ => None,
    }
}

/// The terminal a card names, when it names one: zero is no terminal.
fn card_terminal(card: &AskInfo) -> Option<u32> {
    card.terminal.filter(|t| *t != 0)
}

/// Who asked a card, by the ids the daemon mints: `E` is qass's (a plan when it asks to
/// approve one), `Q` an agent's own question, `W` the daemon's draft: for an agent's stop (it
/// has a task), for a dialog on its screen (a terminal, no task), or a notice about qass (no
/// agent at all).
fn card_kind(card: &AskInfo) -> ItemKind {
    match card.card.chars().next() {
        Some('E') if plan_card(card) => ItemKind::Plan,
        Some('Q') => ItemKind::Agent,
        Some('W') if card.task.is_some() => ItemKind::Agent,
        Some('W') if card_terminal(card).is_some() => ItemKind::Dialog,
        _ => ItemKind::Qass,
    }
}

/// The row a card is about: by its terminal on This Mac, else by its agent's key
/// (`staging/T2`) on another machine.
fn card_row<'a>(card: &AskInfo, rows: &[&'a AgentRow], hand: &HandInput) -> Option<&'a AgentRow> {
    if let Some(t) = card_terminal(card) {
        return rows.iter().copied().find(|r| r.terminal == t);
    }
    let (key, _) = qoop_runtime::asks::agent_key(card.agent.as_deref()?)?;
    rows.iter()
        .copied()
        .find(|r| hand.agent_key(r).as_deref() == Some(key.as_str()))
}

/// What the person answered since `day_start`: the cards with an answer (given and not yet
/// typed, or ended), closed or claimed today, and `own` answers, newest first. A card in both
/// is listed once.
fn answered_today(cards: &[AskInfo], own: &[OwnAnswer], day_start: i64) -> Vec<Answered> {
    let own: Vec<&OwnAnswer> = own.iter().filter(|o| o.at >= day_start).collect();
    let mut out: Vec<Answered> = cards
        .iter()
        .filter(|c| {
            matches!(
                c.status.as_str(),
                "queued" | "answered" | "passed" | "closed"
            )
        })
        .filter(|c| {
            !own.iter()
                .any(|o| o.card.as_deref() == Some(c.card.as_str()))
        })
        .filter_map(|c| {
            Some(Answered {
                at: c.closed_at.filter(|at| *at >= day_start)?,
                label: answer_label(c)?,
                to: c.body.handle.clone(),
                own: false,
            })
        })
        .collect();
    out.extend(own.iter().map(|o| Answered {
        at: o.at,
        label: o.label.clone(),
        to: o.to.clone(),
        own: true,
    }));
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.label.cmp(&b.label)));
    out
}

/// What a card was answered with: the label of the option chosen (the daemon writes its key
/// into `answer` too), else the typed words. `None` for a card closed with no answer.
fn answer_label(card: &AskInfo) -> Option<String> {
    match card.option.as_deref() {
        Some(key) => {
            let named = card.body.options.iter().find(|o| o.key == key);
            Some(named.map_or(key, |o| o.label.as_str()).to_string())
        }
        None => card.answer.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;
    use crate::answers::note_qass;

    /// One state of the golden fixture: the records the daemon sent and what every client must
    /// make of them.
    #[derive(Deserialize)]
    struct State {
        /// The window's clock when the rows were read, for [`note_qass`].
        now: i64,
        rows: Vec<AgentRow>,
        cards: Vec<AskInfo>,
        hand: HandInput,
        context: Context,
        own_answers: Vec<OwnAnswer>,
        expect: Expect,
    }

    #[derive(Deserialize)]
    struct Expect {
        /// The item keys in order.
        order: Vec<String>,
        /// Each item's kind, by key.
        kinds: BTreeMap<String, String>,
        /// The keys of the items that offer Hand to qass.
        hand_to_qass: Vec<String>,
        /// The keys of the items on a machine not reached.
        lost: Vec<String>,
        count: usize,
        with_qass: Vec<u32>,
        finished: Vec<u32>,
        /// Answered today, newest first: `[at, label, to, own]`.
        answered: Vec<(i64, String, String, bool)>,
    }

    #[derive(Deserialize)]
    struct Fixture {
        states: BTreeMap<String, State>,
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("../tests/fixtures/inbox-v3.json")).expect("fixture")
    }

    fn built(state: &State) -> Inbox {
        let mut hand = state.hand.clone();
        hand.set_carded(&state.cards);
        note_qass(&mut hand, &state.rows, state.now);
        Inbox::build(
            &state.rows,
            &state.cards,
            &hand,
            &state.context,
            &state.own_answers,
        )
    }

    /// The design's records in every state give the list, count and sections the design draws.
    #[test]
    fn the_golden_fixture_gives_the_designs_inbox_in_every_state() {
        let fixture = fixture();
        let names: Vec<&str> = fixture.states.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["computer_lost", "first_run", "normal", "qass_stopped"]
        );
        for (name, state) in &fixture.states {
            let inbox = built(state);
            let want = &state.expect;
            let order: Vec<String> = inbox.items.iter().map(Item::key).collect();
            assert_eq!(order, want.order, "{name}: order");
            let kinds: BTreeMap<String, String> = inbox
                .items
                .iter()
                .map(|i| (i.key(), i.kind.token().to_string()))
                .collect();
            assert_eq!(kinds, want.kinds, "{name}: kinds");
            let keys = |f: fn(&Item) -> bool| -> Vec<String> {
                inbox.items.iter().filter(|i| f(i)).map(Item::key).collect()
            };
            assert_eq!(keys(|i| i.hand_to_qass), want.hand_to_qass, "{name}: hand");
            assert_eq!(keys(|i| i.lost), want.lost, "{name}: lost");
            assert_eq!(inbox.count, want.count, "{name}: count");
            assert_eq!(inbox.with_qass, want.with_qass, "{name}: with qass");
            assert_eq!(inbox.finished, want.finished, "{name}: finished");
            let answered: Vec<(i64, String, String, bool)> = inbox
                .answered
                .iter()
                .map(|a| (a.at, a.label.clone(), a.to.clone(), a.own))
                .collect();
            assert_eq!(answered, want.answered, "{name}: answered");
        }
    }

    const REMOTE: i64 = (1 << 40) | 1;

    fn row(terminal: u32, state: &str, status: &str, at: i64) -> AgentRow {
        let mut row: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": terminal, "workspace": 1, "program": "claude", "behind": null,
            "state": state, "generation": 1, "agent": true,
        }))
        .expect("row");
        row.status = Some(status.into());
        row.last_active_at_unix = Some(at);
        row
    }

    fn menu_row(terminal: u32, at: i64) -> AgentRow {
        let mut r = row(terminal, "blocked", "blocked", at);
        r.menu = Some(
            serde_json::from_value(serde_json::json!({
                "question": "Go on?", "seal": 1,
                "options": [{"key": "1", "label": "Yes"}, {"key": "2", "label": "No"}],
            }))
            .expect("menu"),
        );
        r
    }

    /// A card as the daemon stores it: its id, the agent it is about (a terminal and its key,
    /// as `qoop task ask` writes them), its status and when it opened.
    fn card(id: &str, terminal: Option<u32>, status: &str, opened_at: i64) -> AskInfo {
        let mut c: AskInfo = serde_json::from_value(serde_json::json!({
            "card": id, "terminal": terminal, "status": status, "opened_at": opened_at,
            "body": {
                "handle": "api", "urgency": "later", "doing": "a thing", "why": "because",
                "asks": "Go on?", "options": [
                    {"key": "a", "label": "Merge", "reason": "r", "words": "merge"},
                    {"key": "b", "label": "Wait", "reason": "r", "words": "wait"},
                ],
            },
        }))
        .expect("card");
        c.agent = terminal.map(|t| format!("T{t}"));
        c
    }

    fn up() -> HandInput {
        HandInput {
            qass: Some((9, Some(1))),
            watching: Some(true),
            ..HandInput::default()
        }
    }

    fn build(rows: &[AgentRow], cards: &[AskInfo]) -> Inbox {
        let mut hand = up();
        hand.set_carded(cards);
        Inbox::build(rows, cards, &hand, &Context::default(), &[])
    }

    fn keys(inbox: &Inbox) -> Vec<String> {
        inbox.items.iter().map(Item::key).collect()
    }

    /// A blocked row from a machine with no menu in its rows is Open the agent, not a dialog
    /// card that claims to know what it asks.
    #[test]
    fn a_blocked_row_from_an_older_daemon_is_open_not_dialog() {
        let rows = [row(1, "blocked", "blocked", 10)];
        assert_eq!(build(&rows, &[]).items[0].kind, ItemKind::Dialog);
        let ctx = Context {
            old: BTreeSet::from([0]),
            ..Context::default()
        };
        let old = Inbox::build(&rows, &[], &up(), &ctx, &[]);
        assert_eq!(old.items[0].kind, ItemKind::Open);
        assert!(!old.items[0].hand_to_qass);
    }

    /// A card about an agent that waits for words or on a dialog is its one item: the row is
    /// not listed beside it. An older loop comes first.
    #[test]
    fn a_card_replaces_a_reply_or_dialog_row_and_the_list_is_oldest_first() {
        let rows = [
            row(1, "idle", "blocked", 50),
            row(2, "blocked", "blocked", 30),
        ];
        let cards = [
            card("E1", Some(1), "open", 40),
            card("Q2", Some(2), "open", 10),
            card("E3", Some(2), "passed", 5),
        ];
        let got = build(&rows, &cards);
        assert_eq!(keys(&got), ["c:Q2", "c:E1"]);
        assert_eq!(got.count, 2);
        let kinds: Vec<ItemKind> = got.items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [ItemKind::Agent, ItemKind::Qass]);
        // Hand to qass: the card an agent asked, never one qass asked.
        let hands: Vec<bool> = got.items.iter().map(|i| i.hand_to_qass).collect();
        assert_eq!(hands, [true, false]);
    }

    /// A live menu and a failure stay their own items beside a card about the same agent.
    #[test]
    fn a_card_does_not_replace_a_menu_or_a_failure() {
        let rows = [menu_row(1, 20), row(2, "failed", "failed", 30)];
        let cards = [
            card("E1", Some(1), "open", 40),
            card("E2", Some(2), "open", 50),
        ];
        let got = build(&rows, &cards);
        assert_eq!(keys(&got), ["t:1", "t:2", "c:E1", "c:E2"]);
        assert_eq!(got.count, 4);
    }

    /// The ids the daemon mints tell who asked: `E` qass (a plan when it asks to approve one),
    /// `Q` an agent, `W` a stop draft (task), a dialog draft (terminal, no task) or a notice
    /// about qass (neither). Terminal zero is no terminal.
    #[test]
    fn a_card_kind_comes_from_its_id_and_what_it_names() {
        let mut stop = card("W1", Some(1), "open", 1);
        stop.task = Some(7);
        let dialog = card("W2", Some(2), "open", 2);
        let notice = card("W3", None, "open", 3);
        let zero = card("W4", Some(0), "open", 4);
        let mut plan = card("E5", Some(3), "open", 5);
        plan.body.handle = "plan".into();
        plan.item = Some(
            serde_json::from_value(serde_json::json!({"id": 1, "key": "K-1", "plan": "written"}))
                .expect("item"),
        );
        let mut quoted = plan.clone();
        quoted.card = "Q6".into();
        let rows = [
            row(1, "idle", "blocked", 1),
            row(2, "blocked", "blocked", 2),
        ];
        let got = build(&rows, &[stop, dialog, notice, zero, plan, quoted]);
        let kinds: Vec<(String, ItemKind, bool)> = got
            .items
            .iter()
            .map(|i| (i.key(), i.kind, i.hand_to_qass))
            .collect();
        assert_eq!(
            kinds,
            [
                ("c:W1".to_string(), ItemKind::Agent, true),
                ("c:W2".to_string(), ItemKind::Dialog, false),
                ("c:W3".to_string(), ItemKind::Qass, false),
                ("c:W4".to_string(), ItemKind::Qass, false),
                ("c:E5".to_string(), ItemKind::Plan, false),
                ("c:Q6".to_string(), ItemKind::Agent, true),
            ]
        );
        assert_eq!(got.items[3].terminal, None, "terminal zero names no agent");
    }

    /// An agent's `Q` card with its terminal and key: Hand to qass while qass runs, and no
    /// longer when qass leaves that agent alone.
    #[test]
    fn hand_to_qass_on_an_agent_card_follows_qass_and_the_agent() {
        let mut rows = [row(2, "idle", "blocked", 1)];
        let cards = [card("Q2", Some(2), "open", 2)];
        assert!(build(&rows, &cards).items[0].hand_to_qass);
        rows[0].qass_skip = true;
        assert!(!build(&rows, &cards).items[0].hand_to_qass);
    }

    /// A machine not reached keeps its rows as of its last answer: counted, flagged, and
    /// offering no hand-off.
    #[test]
    fn an_item_on_a_machine_not_reached_is_lost() {
        let mut remote = menu_row(0x8100_0001, 10);
        remote.workspace = REMOTE;
        let rows = [remote, menu_row(2, 20)];
        let ctx = Context {
            lost: BTreeSet::from([1]),
            ..Context::default()
        };
        let got = Inbox::build(&rows, &[], &up(), &ctx, &[]);
        let state: Vec<(String, bool, bool)> = got
            .items
            .iter()
            .map(|i| (i.key(), i.lost, i.hand_to_qass))
            .collect();
        assert_eq!(
            state,
            [
                ("t:2164260865".to_string(), true, false),
                ("t:2".to_string(), false, true)
            ]
        );
        assert_eq!(got.count, 2);
    }

    /// A row the daemon has not stamped lists first, never as fresh.
    #[test]
    fn an_unstamped_row_lists_first() {
        let mut fresh = menu_row(3, 0);
        fresh.last_active_at_unix = None;
        let rows = [menu_row(1, 10), menu_row(2, 5), fresh];
        assert_eq!(keys(&build(&rows, &[])), ["t:3", "t:2", "t:1"]);
    }

    /// Qass's own tab is not the person's to answer.
    #[test]
    fn qass_own_space_is_not_listed() {
        let mut qass = menu_row(9, 1);
        qass.workspace = 3;
        let rows = [qass, menu_row(1, 2)];
        let ctx = Context {
            qass_spaces: BTreeSet::from([3]),
            ..Context::default()
        };
        let got = Inbox::build(&rows, &[], &up(), &ctx, &[]);
        assert_eq!(keys(&got), ["t:1"]);
    }

    /// The daemon writes the option's key into `answer` too: an option shows its label, typed
    /// words show as typed, and a card closed with no answer is not listed. A card given an
    /// answer that is not typed yet is listed as well.
    #[test]
    fn answered_today_shows_the_option_label_or_the_typed_words() {
        let mut chosen = card("E1", None, "answered", 10);
        (chosen.answer, chosen.option, chosen.closed_at) =
            (Some("a".into()), Some("a".into()), Some(100));
        let mut typed = card("E2", None, "closed", 10);
        (typed.answer, typed.closed_at) = (Some("use seed 7".into()), Some(90));
        let mut queued = card("E3", None, "queued", 10);
        (queued.answer, queued.option, queued.closed_at) =
            (Some("b".into()), Some("b".into()), Some(110));
        let mut none = card("E4", None, "closed", 10);
        none.closed_at = Some(95);
        let mut early = card("E5", None, "closed", 10);
        (early.answer, early.closed_at) = (Some("old".into()), Some(10));
        let cards = [chosen, typed, queued, none, early];
        let ctx = Context {
            day_start: 50,
            ..Context::default()
        };
        let own = [OwnAnswer {
            at: 105,
            label: "Yes".into(),
            to: "api".into(),
            card: None,
        }];
        let got = Inbox::build(&[], &cards, &up(), &ctx, &own);
        let said: Vec<(i64, &str, bool)> = got
            .answered
            .iter()
            .map(|a| (a.at, a.label.as_str(), a.own))
            .collect();
        assert_eq!(
            said,
            [
                (110, "Wait", false),
                (105, "Yes", true),
                (100, "Merge", false),
                (90, "use seed 7", false),
            ]
        );
    }

    /// Unread finished work and a question qass holds are sections, not part of the count.
    #[test]
    fn finished_and_handed_leave_the_count() {
        let mut handed_row = row(2, "blocked", "blocked", 20);
        handed_row.with_qass = true;
        let rows = [
            row(1, "done", "done", 10),
            handed_row,
            row(3, "idle", "idle", 5),
        ];
        let got = build(&rows, &[]);
        assert_eq!(got.count, 0);
        assert_eq!(
            (got.finished.clone(), got.with_qass.clone()),
            (vec![1], vec![2])
        );
    }
}
