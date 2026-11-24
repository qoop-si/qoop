//! The Board the desktop's Inbox shows as its second view: Waiting on you, Doing, Todo and Done,
//! from `TaskRead { board: true }`. Shared data, read by the desktop; the terminal interface
//! shows the board page in its own pane on qass's screen. The bands and their order are the phone's (`bandsOf` and
//! `group` in `ui/inboxView.ts` and `ui/board.ts`), so the two surfaces list the same items in
//! the same order. Pure values and functions, so a test reads the values instead of a screen.
//!
//! "Waiting on you" holds the same records as the Inbox's cards ("also in Needs you, the same
//! records"): an item is in that band and in no other, and an item whose only waiting agent is
//! With qass is not in it.

use std::collections::BTreeSet;

use qoop_runtime::control::{ControlRequest, ControlResponse, ItemInfo};

/// The request id the board is read under.
pub const READ_ID: &str = "board-read";

/// The id a computer's board is read under: This Mac's is [`READ_ID`], another's names it.
pub fn read_id(machine: usize) -> String {
    match machine {
        0 => READ_ID.to_string(),
        m => format!("{READ_ID}-{m}"),
    }
}

/// What the words under "Waiting on you" say about the records.
pub const WAIT_NOTE: &str = "also in Needs you, the same records";

/// What this client knows of the board.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Read {
    /// Not asked yet.
    #[default]
    Unread,
    /// Asked, and no answer yet.
    Reading,
    /// The board, as the daemon sent it.
    Items(Vec<ItemInfo>),
    /// A daemon from before the board: it answers with no items.
    Old,
    /// The read failed, with the daemon's words.
    Failed(String),
}

/// The board as this client keeps it.
#[derive(Clone, Debug, Default)]
pub struct Board {
    pub read: Read,
    /// A new read is owed: the board moved, or it was never read.
    pub stale: bool,
}

impl Board {
    /// The read to send, when one is owed and none is out.
    pub fn request(&mut self) -> Option<ControlRequest> {
        self.request_via(READ_ID, None)
    }

    /// The same read under `id`, carried to another computer through one of its spaces
    /// (`via`): its answer comes back under `id`, with the numbers in the interface's.
    pub fn request_via(&mut self, id: &str, via: Option<i64>) -> Option<ControlRequest> {
        if !self.stale || self.read == Read::Reading {
            return None;
        }
        self.stale = false;
        if !matches!(self.read, Read::Items(_)) {
            self.read = Read::Reading;
        }
        let read = ControlRequest::TaskRead {
            id: id.to_string(),
            board: true,
            task: None,
            terminal: None,
            token: None,
            before: None,
        };
        Some(match via {
            Some(workspace) => ControlRequest::Via {
                id: format!("via-{id}"),
                workspace,
                request: Box::new(read),
            },
            None => read,
        })
    }

    /// The daemon's answer to the read. True when what is shown changes.
    pub fn answered(&mut self, response: &ControlResponse) -> bool {
        self.answered_to(READ_ID, response)
    }

    /// The answer to the read sent under `read_id`.
    pub fn answered_to(&mut self, read_id: &str, response: &ControlResponse) -> bool {
        match response {
            ControlResponse::Tasks { id, items, .. } if id == read_id => {
                self.read = match items {
                    Some(items) => Read::Items(items.clone()),
                    None => Read::Old,
                };
                true
            }
            ControlResponse::Failed { id, reason } if id == read_id => {
                self.read = if qoop_runtime::control::too_old_for(reason, Some("TaskRead")) {
                    Read::Old
                } else {
                    Read::Failed(reason.clone())
                };
                true
            }
            _ => false,
        }
    }
}

/// What a computer too old for the board is told in: "qoop on staging is too old for the
/// board". `machine` is its name, "This Mac" for this one.
pub fn too_old(machine: &str) -> String {
    format!("qoop on {machine} is too old for the board")
}

/// The four bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Wait,
    Doing,
    Todo,
    Done,
}

impl Band {
    pub const ALL: [Band; 4] = [Band::Wait, Band::Doing, Band::Todo, Band::Done];

    pub fn word(self) -> &'static str {
        match self {
            Band::Wait => "waiting on you",
            Band::Doing => "doing",
            Band::Todo => "todo",
            Band::Done => "done",
        }
    }
}

/// The items in their bands, each sorted as the phone sorts them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bands {
    pub wait: Vec<ItemInfo>,
    pub doing: Vec<ItemInfo>,
    pub todo: Vec<ItemInfo>,
    pub done: Vec<ItemInfo>,
}

impl Bands {
    pub fn of(&self, band: Band) -> &[ItemInfo] {
        match band {
            Band::Wait => &self.wait,
            Band::Doing => &self.doing,
            Band::Todo => &self.todo,
            Band::Done => &self.done,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Todo,
    Doing,
    Done,
}

fn lane(i: &ItemInfo) -> Lane {
    match i.state.as_str() {
        "doing" => Lane::Doing,
        "done" => Lane::Done,
        _ => Lane::Todo,
    }
}

/// Whether the plan of an item waits on the person.
fn plan_waits(i: &ItemInfo) -> bool {
    matches!(i.plan.as_str(), "stale" | "asked")
}

/// Whether an item is among the records of Needs you: it has a question or a plan to approve,
/// or an agent that waits on the person. An agent handed to qass (`handed`: its terminal) is
/// With qass, not in Needs you, so an item that waits on nothing but agents handed is not.
pub fn waits_on_you(i: &ItemInfo, handed: &BTreeSet<u32>) -> bool {
    if lane(i) == Lane::Done {
        return false;
    }
    if i.open_asks > 0 || plan_waits(i) {
        return true;
    }
    i.agents.iter().any(|a| {
        !a.closed
            && a.status == "waits"
            && a.terminal
                .and_then(|t| u32::try_from(t).ok())
                .is_none_or(|t| !handed.contains(&t))
    })
}

/// The moment an item is sorted by: when a Done item closed, else when it last moved.
fn stamp(i: &ItemInfo) -> i64 {
    if lane(i) == Lane::Done {
        i.closed_at
            .or((i.since != 0).then_some(i.since))
            .unwrap_or(i.updated_at)
    } else {
        i.updated_at
    }
}

/// The lanes of `items`, each newest first. The daemon sends ties oldest first, so a lane is
/// reversed before the stable sort, as the phone's `group` does.
fn group(items: &[&ItemInfo]) -> (Vec<ItemInfo>, Vec<ItemInfo>, Vec<ItemInfo>) {
    let mut lanes: [Vec<ItemInfo>; 3] = Default::default();
    for i in items {
        let at = match lane(i) {
            Lane::Todo => 0,
            Lane::Doing => 1,
            Lane::Done => 2,
        };
        lanes[at].push((*i).clone());
    }
    for l in &mut lanes {
        l.reverse();
        l.sort_by_key(|i| std::cmp::Reverse(stamp(i)));
    }
    let [todo, doing, done] = lanes;
    (todo, doing, done)
}

/// The bands: what waits on the person first (Doing before Todo), then Doing, Todo and Done,
/// with what the person put away (hidden) left out.
pub fn bands_of(items: &[ItemInfo], handed: &BTreeSet<u32>) -> Bands {
    let shown: Vec<&ItemInfo> = items.iter().filter(|i| !i.hidden).collect();
    let (waiting, rest): (Vec<&ItemInfo>, Vec<&ItemInfo>) =
        shown.into_iter().partition(|i| waits_on_you(i, handed));
    let (w_todo, w_doing, _) = group(&waiting);
    let (todo, doing, done) = group(&rest);
    Bands {
        wait: w_doing.into_iter().chain(w_todo).collect(),
        doing,
        todo,
        done,
    }
}

/// The plan as one word, "" with none.
fn plan_word(i: &ItemInfo) -> &'static str {
    match i.plan.as_str() {
        "stale" | "asked" => "plan needs you",
        "written" | "changed" => "plan in review",
        _ => "",
    }
}

/// What an agent on the item is doing, as the daemon's token reads in words.
fn agent_word(token: &str) -> &'static str {
    match token {
        "waits" => "an agent waits on you",
        "working" => "working",
        "checks" => "qass checks",
        "asleep" => "asleep",
        "idle" => "idle",
        "error" => "an agent failed",
        "finished" => "finished",
        "closed" => "its tab closed",
        _ => "",
    }
}

/// The words under an item's title: why it waits for the person, or its agent's word, its
/// phase and its steps.
pub fn why(i: &ItemInfo, band: Band) -> String {
    let mut parts: Vec<String> = Vec::new();
    if i.open_asks > 0 {
        parts.push(format!(
            "{} question{}",
            i.open_asks,
            if i.open_asks == 1 { "" } else { "s" }
        ));
    }
    let plan = plan_word(i);
    if !plan.is_empty() && (band == Band::Wait || band == Band::Doing || band == Band::Todo) {
        parts.push(plan.to_string());
    }
    // The first agent still on the item, else the last to leave it.
    let on = i
        .agents
        .iter()
        .find(|a| !a.closed)
        .or_else(|| i.agents.last());
    if let Some(a) = on {
        let word = agent_word(&a.status);
        if !word.is_empty() && !(band == Band::Wait && i.open_asks > 0 && a.status == "waits") {
            parts.push(word.to_string());
        }
    }
    if band == Band::Done
        && let Some(outcome) = i.outcome.as_deref().filter(|o| !o.trim().is_empty())
    {
        parts.push(outcome.trim().to_string());
    }
    if let Some(phase) = &i.phase {
        parts.push(format!("{} {}/{}", phase.name, phase.n, phase.of));
    }
    if i.steps_total > 0 {
        parts.push(format!("{}/{} steps", i.steps_done, i.steps_total));
    }
    if parts.is_empty() && band == Band::Wait {
        "needs you".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, state: &str, at: i64, extra: serde_json::Value) -> ItemInfo {
        let mut v = serde_json::json!({
            "id": at, "key": key, "workspace": null, "title": format!("title of {key}"),
            "state": state, "steps_done": 0, "steps_total": 0, "agents": [],
            "updated_at": at,
        });
        for (k, val) in extra.as_object().cloned().unwrap_or_default() {
            v[k] = val;
        }
        serde_json::from_value(v).expect("item")
    }

    fn keys(items: &[ItemInfo]) -> Vec<&str> {
        items.iter().map(|i| i.key.as_str()).collect()
    }

    /// The design's board: two items wait on the person, two are doing, one is todo, one is
    /// done; each is in one band, newest first inside it, and the waiting band is the same
    /// records as Needs you.
    #[test]
    fn items_fall_into_the_bands_in_the_phones_order() {
        let items = vec![
            item("QOOP-66", "done", 10, serde_json::json!({"closed_at": 12})),
            item("QOOP-68", "doing", 20, serde_json::json!({"open_asks": 1})),
            item("QOOP-71", "doing", 30, serde_json::json!({})),
            item("QOOP-72", "todo", 40, serde_json::json!({})),
            item("QOOP-73", "todo", 50, serde_json::json!({"plan": "asked"})),
            item(
                "QOOP-74",
                "doing",
                25,
                serde_json::json!({"agents": [{"task": 1, "terminal": 7, "name": "a",
                    "machine": "this Mac", "stage": "x", "status": "waits"}]}),
            ),
            item("QOOP-75", "doing", 60, serde_json::json!({"hidden": true})),
        ];
        let bands = bands_of(&items, &BTreeSet::new());
        // Doing before Todo inside the waiting band, newest first.
        assert_eq!(keys(&bands.wait), ["QOOP-74", "QOOP-68", "QOOP-73"]);
        assert_eq!(keys(&bands.doing), ["QOOP-71"]);
        assert_eq!(keys(&bands.todo), ["QOOP-72"]);
        assert_eq!(keys(&bands.done), ["QOOP-66"]);
        let all: usize = Band::ALL.iter().map(|b| bands.of(*b).len()).sum();
        assert_eq!(all, 6, "one band each, and the hidden item in none");
    }

    /// An item whose only waiting agent is handed to qass is With qass, so it is not in
    /// Needs you and not in the waiting band either.
    #[test]
    fn an_item_whose_agent_is_with_qass_is_not_waiting_on_you() {
        let waiting = item(
            "QOOP-74",
            "doing",
            25,
            serde_json::json!({"agents": [{"task": 1, "terminal": 7, "name": "a",
                "machine": "this Mac", "stage": "x", "status": "waits"}]}),
        );
        assert!(waits_on_you(&waiting, &BTreeSet::new()));
        assert!(!waits_on_you(&waiting, &BTreeSet::from([7])));
        let bands = bands_of(&[waiting], &BTreeSet::from([7]));
        assert!(bands.wait.is_empty() && keys(&bands.doing) == ["QOOP-74"]);
        // A question of its own keeps it in Needs you whoever holds the agent.
        let asked = item("Q-1", "doing", 1, serde_json::json!({"open_asks": 2}));
        assert!(waits_on_you(&asked, &BTreeSet::from([7])));
    }

    /// The board is read once when asked for and again only after it moved.
    #[test]
    fn the_board_is_read_when_stale_and_a_daemon_without_items_is_too_old() {
        let mut board = Board::default();
        assert!(
            board.request().is_none(),
            "nothing is read until it is asked for"
        );
        board.stale = true;
        assert!(matches!(
            board.request(),
            Some(ControlRequest::TaskRead { board: true, .. })
        ));
        assert!(board.request().is_none(), "one read at a time");
        board.stale = true;
        assert!(board.request().is_none(), "still reading");
        let said = ControlResponse::Tasks {
            id: READ_ID.into(),
            tasks: vec![],
            events: vec![],
            more: false,
            space: None,
            items: None,
            item: None,
            loose: None,
        };
        assert!(board.answered(&said));
        assert_eq!(board.read, Read::Old);
    }
}
