//! `qoop doctor`: whether the board, qass's questions and the agents tell one truth.
//!
//! Read-only on the store: four reads through the daemon (the agents, the board, the open
//! tasks, the live cards), checked against the sync invariants. One line per finding,
//! nothing when all hold. It never acts: qass, or the owner, does what a line says.
//! Exit 0 when clean, 1 with findings, 3 when a read could not be made (a daemon from before
//! it, or one that did not answer), each skipped check named so silence never means "not
//! checked". `--new <file>` prints only findings whose id is not in the file and writes the
//! current ids there, so a known finding is said once.

use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::time::Duration;

use qoop_runtime::control::{
    AgentRow, AskInfo, ControlRequest, ControlResponse, ItemInfo, Status, TaskInfo,
};

/// The space qass and its inbox live in.
const QASS_SPACE: &str = "qass";

/// How long one read may take before it counts as "cannot check".
const PATIENCE: Duration = Duration::from_secs(10);

/// One thing that does not hold: a stable id (for `--new`) and the line said about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub id: String,
    pub line: String,
}

/// The spaces as the doctor needs them: which is qass's, and which tabs are asleep (a sleeping
/// tab is not in the agents listing, and sleep closes nothing).
#[derive(Debug, Clone, Default)]
pub(crate) struct Spaces {
    pub qass: Option<i64>,
    pub asleep: BTreeSet<u32>,
}

/// What the daemon said, each read on its own: an error is the reason it could not be read.
pub(crate) struct Seen {
    pub agents: Result<Vec<AgentRow>, String>,
    pub spaces: Result<Spaces, String>,
    pub board: Result<Option<Vec<ItemInfo>>, String>,
    /// Every open task, each with the board's status; `tasks_cut` when they did not fit one
    /// answer.
    pub tasks: Result<Vec<TaskInfo>, String>,
    pub tasks_cut: bool,
    pub asks: Result<Vec<AskInfo>, String>,
}

/// Every finding, and every check that could not run.
pub(crate) fn check(seen: &Seen) -> (Vec<Finding>, Vec<String>) {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let agents = match &seen.agents {
        Ok(rows) => Some(rows),
        Err(e) => {
            skipped.push(format!("cannot check the agents: {e}"));
            None
        }
    };
    let spaces = match &seen.spaces {
        Ok(spaces) => Some(spaces.clone()),
        Err(e) => {
            skipped.push(format!(
                "cannot check the spaces (which is qass's, which tabs sleep): {e}"
            ));
            None
        }
    };
    let asks = match &seen.asks {
        Ok(asks) => Some(asks),
        Err(e) => {
            skipped.push(format!("cannot check the questions: {e}"));
            None
        }
    };
    match (&seen.board, agents) {
        (Ok(Some(items)), Some(rows)) => found.extend(closed_items(
            items,
            rows,
            &spaces
                .as_ref()
                .map(|s| s.asleep.clone())
                .unwrap_or_default(),
        )),
        (Ok(None), _) => skipped.push("cannot check the board: this daemon keeps none".into()),
        (Err(e), _) => skipped.push(format!("cannot check the board: {e}")),
        (Ok(Some(_)), None) => {}
    }
    let tasks = match &seen.tasks {
        Ok(tasks) => Some(tasks),
        Err(e) => {
            skipped.push(format!("cannot check the open tasks: {e}"));
            None
        }
    };
    if seen.tasks_cut {
        skipped.push("the open tasks did not fit one answer: I4 saw only the oldest".into());
    }
    // Both need to know qass's space and the sleeping tabs: without them qass's own tab and a
    // sleeping one would read as findings.
    if let (Some(rows), Some(asks), Some(spaces)) = (agents, asks, &spaces) {
        if let Some(tasks) = tasks {
            found.extend(waiting_without_card(tasks, rows, asks, spaces));
        }
        found.extend(cards_with_nobody(rows, asks, spaces));
    }
    (found, skipped)
}

/// I1: a closed item keeps no open task that is claimed, or whose agent is gone or asleep.
/// Remote agents are their machine's to check.
fn closed_items(items: &[ItemInfo], rows: &[AgentRow], asleep: &BTreeSet<u32>) -> Vec<Finding> {
    let live: HashMap<i64, &AgentRow> = rows.iter().map(|r| (i64::from(r.terminal), r)).collect();
    let mut found = Vec::new();
    for item in items.iter().filter(|i| i.state == "done") {
        let outcome = item.outcome.as_deref().unwrap_or("closed");
        for a in item
            .agents
            .iter()
            .filter(|a| !a.closed && a.machine == "this Mac")
        {
            let terminal = a.terminal.and_then(|t| u32::try_from(t).ok());
            let why = if a.claimed {
                "claimed done"
            } else if terminal.is_some_and(|t| asleep.contains(&t)) {
                "asleep"
            } else if a.terminal.is_none_or(|t| !live.contains_key(&t)) {
                "its tab is gone"
            } else {
                continue;
            };
            found.push(Finding {
                id: format!("I1:{}:{}", item.key, a.task),
                line: format!(
                    "I1 {}: closed {outcome}, but task {} ({}) is still open and {why}; close it: \
                     qoop task close {} --{}",
                    item.key,
                    a.task,
                    a.name,
                    a.task,
                    if outcome == "failed" {
                        "failed"
                    } else {
                        "verified"
                    },
                ),
            });
        }
    }
    found
}

/// Whether a card is live: open, or answered and on its way.
fn live(a: &AskInfo) -> bool {
    matches!(a.status.as_str(), "open" | "queued" | "passed")
}

/// I4: a task that waits on the owner has a live card, wherever its tab is: live (once its
/// agent is at rest), asleep, or gone. Waiting is the board's own word for it (`waits` from
/// `control::status`, where `waiting-human` comes before a claim), so a claimed task that
/// asks is seen too. Only `waiting-human` asks for one: a plain claim is checked by qass (it may
/// read `waits` when nobody can check it, but a claim never becomes his card by itself), and
/// a live tab on a menu reads `waits` for a menu qass answers. qass's own space is skipped:
/// it asks him through cards it writes itself.
fn waiting_without_card(
    tasks: &[TaskInfo],
    rows: &[AgentRow],
    asks: &[AskInfo],
    spaces: &Spaces,
) -> Vec<Finding> {
    let live_cards: Vec<&AskInfo> = asks.iter().filter(|a| live(a)).collect();
    tasks
        .iter()
        .filter(|t| t.workspace.is_none_or(|w| Some(w) != spaces.qass))
        .filter(|t| waits_on_owner(t))
        .filter_map(|t| {
            let terminal = t.terminal.and_then(|n| u32::try_from(n).ok());
            let carded = live_cards
                .iter()
                .any(|a| a.task == Some(t.id) || (a.terminal.is_some() && a.terminal == terminal));
            if carded {
                return None;
            }
            let row = terminal.and_then(|n| rows.iter().find(|r| r.terminal == n));
            let tab = match (row, terminal) {
                // At rest: the inbox's draft comes when the turn ends, not when the stage is set.
                (Some(r), _) if matches!(r.state.as_str(), "working" | "starting") => {
                    return None;
                }
                (Some(_), _) => "",
                (None, Some(n)) if spaces.asleep.contains(&n) => ", its tab asleep,",
                _ => ", its tab gone,",
            };
            let claimed = if t.claimed_at.is_some() {
                " and claimed done"
            } else {
                ""
            };
            let (head, who) = match (terminal, row) {
                (Some(n), Some(r)) => (format!("T{n}"), format!(" {}", name(r))),
                (Some(n), None) => (format!("T{n}"), String::new()),
                (None, _) => ("-".to_string(), String::new()),
            };
            Some(Finding {
                id: format!("I4:{head}:{}", t.id),
                line: format!(
                    "I4 {head}{who}: waits on you (task {}{claimed}){tab} with no question card; \
                     qass writes one",
                    t.id
                ),
            })
        })
        .collect()
}

/// Whether the board says a task waits on him: `waiting-human` and the board's `waits` (an
/// older daemon sends no status; its stage alone then).
fn waits_on_owner(t: &TaskInfo) -> bool {
    t.stage == "waiting-human" && (t.status.is_empty() || t.status == Status::Waits.as_str())
}

/// I6: a live card has someone to take its answer: its agent's tab, else qass's.
fn cards_with_nobody(rows: &[AgentRow], asks: &[AskInfo], spaces: &Spaces) -> Vec<Finding> {
    let open: BTreeSet<u32> = rows.iter().map(|r| r.terminal).collect();
    let qass_tab = spaces
        .qass
        .is_some_and(|q| rows.iter().any(|r| r.workspace == q && r.agent));
    asks.iter()
        .filter(|a| live(a))
        .filter_map(|a| {
            let why = match a.terminal {
                Some(t) if !open.contains(&t) && !spaces.asleep.contains(&t) => {
                    format!("its tab T{t} is closed")
                }
                None if !qass_tab => "it is about no tab and qass has none".to_string(),
                _ => return None,
            };
            Some(Finding {
                id: format!("I6:{}", a.card),
                line: format!(
                    "I6 {} ({}): {why}, so his answer has nowhere to go; qass asks it again in \
                     a card of its own, or closes it only when the question is moot: \
                     escalate.sh done {}",
                    a.card, a.body.handle, a.card
                ),
            })
        })
        .collect()
}

fn name(r: &AgentRow) -> String {
    r.name
        .clone()
        .unwrap_or_else(|| format!("terminal {}", r.terminal))
}

/// The four reads, each on its own: one that fails does not stop the others.
fn read() -> Seen {
    let before = read_spaces();
    let agents = match crate::task::ask(
        ControlRequest::ListAgents {
            id: crate::request_id("doctor"),
        },
        PATIENCE,
    ) {
        Ok(ControlResponse::Agents { agents, .. }) => Ok(agents),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    // Read on both sides of the agents: a tab woken in between is in one of the two, never
    // missing from both, so it is not taken for a closed one.
    let after = read_spaces();
    let spaces = match (before, after) {
        (Ok(mut a), Ok(b)) => {
            a.asleep.extend(b.asleep);
            Ok(a)
        }
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    let board = match crate::task::ask(
        ControlRequest::TaskRead {
            id: crate::request_id("doctor"),
            board: true,
            task: None,
            terminal: None,
            token: None,
            before: None,
        },
        PATIENCE,
    ) {
        Ok(ControlResponse::Tasks { items, .. }) => Ok(items),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    // Every open task, wherever its tab is: the agents listing has only the live ones.
    let open = match crate::task::ask(
        ControlRequest::TaskRead {
            id: crate::request_id("doctor"),
            board: false,
            task: None,
            terminal: None,
            token: None,
            before: None,
        },
        PATIENCE,
    ) {
        Ok(ControlResponse::Tasks { tasks, more, .. }) => Ok((tasks, more)),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    let tasks_cut = open.as_ref().is_ok_and(|(_, more)| *more);
    let tasks = open.map(|(tasks, _)| tasks);
    let asks = match crate::task::ask(
        ControlRequest::AskRead {
            id: crate::request_id("doctor"),
            all: false,
        },
        PATIENCE,
    ) {
        Ok(ControlResponse::Asks { live, .. }) => Ok(live),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    };
    Seen {
        agents,
        spaces,
        board,
        tasks,
        tasks_cut,
        asks,
    }
}

/// Which space is qass's, and which tabs sleep.
fn read_spaces() -> Result<Spaces, String> {
    match crate::task::ask(
        ControlRequest::ListWorkspaces {
            id: crate::request_id("doctor"),
        },
        PATIENCE,
    ) {
        Ok(ControlResponse::Workspaces { workspaces, .. }) => Ok(Spaces {
            qass: workspaces
                .iter()
                .find(|w| w.name == QASS_SPACE)
                .map(|w| w.id),
            asleep: workspaces
                .iter()
                .flat_map(|w| w.sleeping.iter().map(|t| t.terminal))
                .collect(),
        }),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// The findings not said before, by id; the file then holds every current id, so a finding
/// that went away and comes back is said again.
///
/// When a check could not run (`partial`), its old ids are kept: a finding it did not see this
/// time has not gone away.
fn only_new(
    found: Vec<Finding>,
    file: &std::path::Path,
    partial: bool,
) -> std::io::Result<(Vec<Finding>, Vec<String>)> {
    let said: BTreeSet<String> = match std::fs::read_to_string(file) {
        Ok(text) => text.lines().map(str::to_string).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        // One it cannot read says every finding as new: never fewer than there are.
        Err(e) => {
            eprintln!("qoop doctor: {} could not be read: {e}", file.display());
            BTreeSet::new()
        }
    };
    let mut now: BTreeSet<String> = found.iter().map(|f| f.id.clone()).collect();
    if partial {
        now.extend(said.iter().cloned());
    }
    let new = found
        .into_iter()
        .filter(|f| !said.contains(&f.id))
        .collect();
    Ok((new, now.into_iter().collect()))
}

/// Keep `ids` in `file`, after the findings were printed: a failure to keep them is said and
/// changes no exit code. Written whole through a rename from a name of this run's own.
fn keep(ids: &[String], file: &std::path::Path) {
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".new.{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let body: String = ids.iter().map(|id| format!("{id}\n")).collect();
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, file)) {
        let _ = std::fs::remove_file(&tmp);
        eprintln!(
            "qoop doctor: the findings were not kept in {}: {e}",
            file.display()
        );
    }
}

const HELP: &str = "\
usage: qoop doctor [--json] [--new <file>]

Whether the board, qass's questions and the agents tell one truth. Read-only: it reads the
agents, the board, the open tasks and the live cards through the daemon and prints one line
per thing that does not hold, and nothing when all hold. It never acts.

  I1  an item closed in the last day still has an open task that is claimed, or whose tab is
      gone or asleep
  I4  a task waits on you (waiting-human, claimed done or not; its tab at rest, asleep or
      gone) and has no question card
  I6  a live card has nobody to take its answer: its tab closed, or no tab and no qass

  --json        the findings as JSON: [{\"id\", \"line\"}], skipped checks under \"skipped\"
  --new <file>  only findings not in the file (and the exit code counts only those); the
                file then holds the current ids

Exit 0 clean, 1 findings, 3 a read could not be made (each skipped check is named).
Limits: live cards only; this Mac only; items closed in the last day (the board's Done
lane); a question on an agent's screen it has not ended its turn with is not seen.
";

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest = &args[args.len().min(2)..];
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    const USAGE: &str = "usage: qoop doctor [--json] [--new <file>]";
    let mut json = false;
    let mut new: Option<&str> = None;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--json" if !json => json = true,
            "--new" if new.is_none() => {
                let file = words
                    .next()
                    .filter(|f| !f.starts_with('-'))
                    .ok_or_else(|| {
                        crate::usage(format!("qoop doctor: --new names a file; {USAGE}"))
                    })?;
                new = Some(file);
            }
            other => {
                return Err(crate::usage(format!(
                    "qoop doctor: unexpected {other}; {USAGE}"
                )));
            }
        }
    }
    let (found, skipped) = check(&read());
    let (found, keep_ids) = match new {
        Some(file) => {
            let (new, ids) = only_new(found, std::path::Path::new(file), !skipped.is_empty())?;
            (new, Some((ids, file)))
        }
        None => (found, None),
    };
    let mut out = std::io::stdout().lock();
    if json {
        let body = serde_json::json!({
            "findings": found
                .iter()
                .map(|f| serde_json::json!({ "id": f.id, "line": f.line }))
                .collect::<Vec<_>>(),
            "skipped": skipped,
        });
        writeln!(out, "{body}")?;
    } else {
        for f in &found {
            writeln!(out, "{}", f.line)?;
        }
        for s in &skipped {
            writeln!(out, "{s}")?;
        }
    }
    out.flush()?;
    drop(out);
    if let Some((ids, file)) = keep_ids {
        keep(&ids, std::path::Path::new(file));
    }
    if !skipped.is_empty() {
        std::process::exit(3);
    }
    if !found.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::control::{AskCard, ItemAgentInfo, Live, StatusInput, Tab, TaskRef, status};

    fn row(terminal: u32, workspace: i64, task: Option<(i64, &str, bool)>) -> AgentRow {
        let mut r: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": terminal,
            "workspace": workspace,
            "state": "idle",
            "readiness": "ready",
            "agent": true,
        }))
        .expect("a minimal row");
        r.name = Some(format!("agent{terminal}"));
        r.task = task.map(|(id, stage, claimed)| TaskRef {
            id,
            goal: "g".into(),
            stage: stage.into(),
            claimed,
        });
        r
    }

    fn item(key: &str, state: &str, outcome: Option<&str>, agents: Vec<ItemAgentInfo>) -> ItemInfo {
        let mut i: ItemInfo = serde_json::from_value(serde_json::json!({
            "id": 1, "key": key, "title": "t", "state": state,
            "steps_done": 0, "steps_total": 0, "agents": [], "updated_at": 0,
        }))
        .expect("a minimal item");
        i.outcome = outcome.map(str::to_string);
        i.agents = agents;
        i
    }

    fn agent(task: i64, terminal: Option<i64>, claimed: bool, closed: bool) -> ItemAgentInfo {
        let mut a: ItemAgentInfo = serde_json::from_value(serde_json::json!({
            "task": task, "name": "worker", "machine": "this Mac", "stage": "implementing",
            "claimed": claimed, "closed": closed,
        }))
        .expect("a minimal agent");
        a.terminal = terminal;
        a
    }

    fn card(card: &str, terminal: Option<u32>, status: &str) -> AskInfo {
        let mut a: AskInfo = serde_json::from_value(serde_json::json!({
            "card": card, "status": status, "opened_at": 0,
            "body": serde_json::to_value(AskCard {
                handle: "dub".into(),
                urgency: "now".into(),
                asked: None,
                doing: "d".into(),
                why: "w".into(),
                asks: "a".into(),
                options: Vec::new(),
                pick: None,
            }).expect("a card"),
        }))
        .expect("a minimal card");
        a.terminal = terminal;
        a
    }

    /// Where a task's tab is, for its status.
    #[derive(Clone, Copy)]
    enum At {
        Live,
        Asleep,
        Gone,
    }

    /// An open task as the daemon sends it, its status made by the board's own `status`.
    fn task(
        id: i64,
        terminal: Option<i64>,
        workspace: i64,
        stage: &str,
        claimed: bool,
        at: At,
    ) -> TaskInfo {
        let tab = match at {
            At::Live => Tab {
                live: Some(Live {
                    state: "idle",
                    last_active: None,
                    subagents: 0,
                }),
                ..Default::default()
            },
            At::Asleep => Tab {
                asleep_since: Some(0),
                ..Default::default()
            },
            At::Gone => Tab::default(),
        };
        let claimed_at = claimed.then_some(100);
        let (token, at) = status(StatusInput {
            stage,
            claimed_at,
            updated_at: 100,
            tab,
            ..Default::default()
        });
        let mut t: TaskInfo = serde_json::from_value(serde_json::json!({
            "id": id, "goal": "g", "stage": stage, "opened_at": 0, "updated_at": 100,
        }))
        .expect("a minimal task");
        t.terminal = terminal;
        t.workspace = Some(workspace);
        t.claimed_at = claimed_at;
        t.status = token.as_str().to_string();
        t.status_at = at;
        t
    }

    /// The open tasks of the live rows, as the daemon would send them beside the listing.
    fn tasks_of(rows: &[AgentRow]) -> Vec<TaskInfo> {
        rows.iter()
            .filter_map(|r| {
                let t = r.task.as_ref()?;
                Some(task(
                    t.id,
                    Some(i64::from(r.terminal)),
                    r.workspace,
                    &t.stage,
                    t.claimed,
                    At::Live,
                ))
            })
            .collect()
    }

    fn seen(agents: Vec<AgentRow>, board: Vec<ItemInfo>, asks: Vec<AskInfo>) -> Seen {
        Seen {
            tasks: Ok(tasks_of(&agents)),
            tasks_cut: false,
            agents: Ok(agents),
            spaces: Ok(Spaces {
                qass: Some(9),
                asleep: BTreeSet::from([6]),
            }),
            board: Ok(Some(board)),
            asks: Ok(asks),
        }
    }

    #[test]
    fn a_store_that_tells_one_truth_has_nothing_to_say() {
        let (found, skipped) = check(&seen(
            vec![
                row(5, 1, Some((50, "implementing", false))),
                row(7, 9, None),
            ],
            vec![item(
                "QOOP-1",
                "doing",
                None,
                vec![agent(50, Some(5), false, false)],
            )],
            vec![card("E1", Some(5), "open")],
        ));
        assert_eq!((found, skipped), (Vec::new(), Vec::<String>::new()));
    }

    #[test]
    fn a_closed_item_with_a_claimed_gone_or_asleep_task_is_said() {
        // T6 is asleep: listed under its space, not among the agents.
        let rows = vec![row(5, 1, Some((60, "implementing", false)))];
        let items = vec![item(
            "QOOP-3",
            "done",
            Some("verified"),
            vec![
                agent(60, Some(5), true, false),  // claimed, tab live
                agent(61, Some(6), false, false), // asleep
                agent(62, Some(8), false, false), // tab gone
                agent(63, Some(5), false, false), // working on: not said
                agent(64, None, false, true),     // closed: not said
            ],
        )];
        let (found, _) = check(&seen(rows, items, Vec::new()));
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["I1:QOOP-3:60", "I1:QOOP-3:61", "I1:QOOP-3:62"]);
        assert!(found[0].line.contains("claimed done"), "{}", found[0].line);
        assert!(found[0].line.contains("qoop task close 60 --verified"));
    }

    #[test]
    fn an_agent_waiting_on_him_with_no_live_card_is_said_and_qass_is_not() {
        let rows = vec![
            row(5, 1, Some((50, "waiting-human", false))), // no card: said
            row(6, 1, Some((51, "waiting-human", false))), // queued answer: live
            row(7, 1, Some((52, "waiting-human", true))),  // claimed and asking: said
            row(8, 9, Some((53, "waiting-human", false))), // qass's own tab
        ];
        let asks = vec![card("W1", Some(6), "queued"), card("E2", Some(5), "closed")];
        let (found, _) = check(&seen(rows, Vec::new(), asks));
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["I4:T5:50", "I4:T7:52"]);
    }

    /// F1: a claimed and an unclaimed `waiting-human` task ask for a card, on a live, an
    /// asleep (T6) and a gone tab alike, except a claim with no tab, which qass closes; a
    /// plain done claim never does, wherever it is.
    #[test]
    fn a_wait_asks_for_a_card_on_every_tab_and_a_plain_claim_never_does() {
        let rows = vec![row(5, 1, None)];
        let cases = [
            ("waiting-human", true, true),
            ("waiting-human", false, true),
            ("verifying", true, false),
        ];
        for (stage, claimed, asks_him) in cases {
            for (terminal, at) in [(5, At::Live), (6, At::Asleep), (8, At::Gone)] {
                let open = vec![task(70, Some(terminal), 1, stage, claimed, at)];
                let mut s = seen(rows.clone(), Vec::new(), Vec::new());
                s.tasks = Ok(open.clone());
                let (found, _) = check(&s);
                let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
                let want: &[String] = &[format!("I4:T{terminal}:70")];
                let case = format!("{stage} claimed={claimed} T{terminal}");
                if asks_him && !(claimed && terminal == 8) {
                    assert_eq!(ids, want, "{case}");
                    let line = &found[0].line;
                    assert_eq!(line.contains("claimed done"), claimed, "{case}: {line}");
                    assert_eq!(line.contains("asleep"), terminal == 6, "{case}: {line}");
                    assert_eq!(line.contains("gone"), terminal == 8, "{case}: {line}");
                    // A live card about its task, or its tab, settles it.
                    let mut by_task = card("E5", None, "open");
                    by_task.task = Some(70);
                    for c in [by_task, card("E6", Some(terminal as u32), "open")] {
                        let mut s = seen(rows.clone(), Vec::new(), vec![c]);
                        s.tasks = Ok(open.clone());
                        let (found, _) = check(&s);
                        assert!(
                            found.iter().all(|f| !f.id.starts_with("I4")),
                            "{case}: {found:?}"
                        );
                    }
                } else {
                    assert!(ids.is_empty(), "{case}: {found:?}");
                }
            }
        }
        // A tab-less wait is said too.
        let mut s = seen(rows, Vec::new(), Vec::new());
        s.tasks = Ok(vec![task(71, None, 1, "waiting-human", false, At::Gone)]);
        let (found, _) = check(&s);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "I4:-:71");
    }

    #[test]
    fn a_live_card_with_nobody_to_answer_it_is_said() {
        let rows = vec![row(5, 1, None)];
        let asks = vec![
            card("E1", Some(5), "open"),  // its tab is here
            card("E2", Some(77), "open"), // its tab closed
            card("E3", None, "passed"),   // no tab, and qass has none
            card("E4", Some(6), "open"),  // its tab is asleep: not closed
        ];
        let (found, _) = check(&seen(rows.clone(), Vec::new(), asks.clone()));
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["I6:E2", "I6:E3"]);
        // With qass's tab up, a card about no tab has someone to take it.
        let mut with_qass = rows;
        with_qass.push(row(9, 9, None));
        let (found, _) = check(&seen(with_qass, Vec::new(), asks));
        let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["I6:E2"]);
    }

    #[test]
    fn a_read_that_failed_names_the_checks_it_skipped() {
        let (found, skipped) = check(&Seen {
            agents: Err("the daemon did not answer in time".into()),
            spaces: Ok(Spaces::default()),
            tasks: Err("the daemon did not answer in time".into()),
            tasks_cut: false,
            board: Ok(None),
            asks: Err("unknown variant `AskRead`".into()),
        });
        assert!(found.is_empty());
        assert_eq!(skipped.len(), 4, "{skipped:?}");
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("cannot check the agents"))
        );
        assert!(skipped.iter().any(|s| s.contains("cannot check the board")));
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("cannot check the open tasks"))
        );
        assert!(
            skipped
                .iter()
                .any(|s| s.contains("cannot check the questions"))
        );
    }

    #[test]
    fn without_the_spaces_no_card_or_wait_is_judged() {
        let (found, skipped) = check(&Seen {
            agents: Ok(vec![row(5, 1, Some((50, "waiting-human", false)))]),
            spaces: Err("timed out".into()),
            tasks: Ok(Vec::new()),
            tasks_cut: false,
            board: Ok(Some(Vec::new())),
            asks: Ok(vec![card("E1", Some(77), "open")]),
        });
        assert!(found.is_empty(), "{found:?}");
        assert!(
            skipped[0].contains("cannot check the spaces"),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_finding_is_said_once_until_it_goes_away() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("said");
        let f = |id: &str| Finding {
            id: id.into(),
            line: id.into(),
        };
        let new = |found: Vec<Finding>, partial| {
            let (new, ids) = only_new(found, &file, partial).unwrap();
            keep(&ids, &file);
            new
        };
        assert_eq!(new(vec![f("a"), f("b")], false).len(), 2);
        assert_eq!(new(vec![f("a"), f("b")], false).len(), 0);
        // A run that could not check everything forgets nothing.
        assert_eq!(new(vec![f("b")], true).len(), 0);
        assert_eq!(new(vec![f("a"), f("b")], false).len(), 0);
        assert_eq!(new(vec![f("b")], false).len(), 0, "a went away");
        assert_eq!(
            new(vec![f("a"), f("b")], false),
            vec![f("a")],
            "a came back"
        );
        // A file it cannot write leaves the findings said and no stray temp file.
        keep(&["x".into()], dir.path());
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }
}
