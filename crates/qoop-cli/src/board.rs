//! `qoop task board`: the board's items, then every space with the owner's goal for it and
//! the open tasks in it, as Markdown a person or qass can read; `--json`, the items alone.
//!
//! Read from the record itself, as `qoop ls` is: no frame to fit in, no events cut, nothing
//! held open on the daemon. Only the agents' live names come from the daemon, through an
//! observer connection, and a daemon that is not there leaves the stored names. With `--dir`
//! the same text is a folder: `README.md` and one `space-<id>.md` per space (ids, because two
//! spaces may share a name), rewritten whole each time, so a copy never drifts from the
//! record; it only ages, and says how old it is.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use crate::qass_page::ago;

use qoop_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, ItemInfo, Live, Status, agent_name, item_info,
    tab_of, task_status,
};
use qoop_runtime::{client, paths};
use qoop_store::task::one_line;
use qoop_store::{Store, TaskRow};

/// The space qass lives in: its own files are not the owner's work.
const QASS_SPACE: &str = crate::qass::SPACE;

/// How far back the closed list looks, and how many it shows.
const CLOSED_SECS: i64 = 86_400;
const CLOSED_MAX: usize = 5;

/// The newest evidence lines shown per task.
const EVIDENCE_SHOWN: usize = 3;

struct Space {
    id: i64,
    name: String,
    goal: Option<String>,
    tasks: Vec<Open>,
}

struct Open {
    task: TaskRow,
    name: String,
    /// What its agent is doing and since when, as the board's other readers word it.
    status: Status,
    status_at: i64,
    /// When it moved to its stage: its newest stage event, else its opening.
    staged: i64,
    plan: Option<String>,
    evidence: Vec<(i64, String)>,
    claim: Option<String>,
}

struct Board {
    asof: String,
    now: i64,
    items: Vec<ItemInfo>,
    /// A closed item's closing note, by its id: what the closer checked.
    notes: HashMap<i64, String>,
    spaces: Vec<Space>,
    closed: Vec<Closed>,
}

struct Closed {
    task: TaskRow,
    space: String,
    /// What the closer wrote, qass's note of what it checked for a verified close.
    note: String,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The local time, as the machine's `date` says it: no time zone table in this binary.
fn local_now() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("unix {}", now()))
}

/// The live rows of this machine's agents, by terminal: their sidebar names and what each tab
/// is doing, and whether qass can check a claim now (the daemon's snapshot, which lives only
/// in the daemon). An older daemon sends none, and its claims read checked as it shows them;
/// with no daemon at all nothing can check, so empty and false.
fn live_rows() -> (HashMap<i64, AgentRow>, bool) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return (HashMap::new(), false);
    };
    rt.block_on(async {
        let Ok(dir) = paths::runtime_dir() else {
            return (HashMap::new(), false);
        };
        let Ok((stream, inbuf)) =
            client::connect_observer(&paths::socket_path(&dir), "qoop-board").await
        else {
            return (HashMap::new(), false);
        };
        let request = ControlRequest::ListAgents {
            id: crate::request_id("board"),
        };
        match crate::ask_over(stream, inbuf, &request, crate::ASK_PATIENCE).await {
            Ok(ControlResponse::Agents {
                agents,
                qass_checks,
                ..
            }) => (
                agents
                    .into_iter()
                    .map(|a| (i64::from(a.terminal), a))
                    .collect(),
                qass_checks.unwrap_or(true),
            ),
            _ => (HashMap::new(), false),
        }
    })
}

/// What the closer of a task or item wrote, qass's note of what it checked for a verified close.
fn closing_note(store: &Store, task: i64) -> std::io::Result<String> {
    Ok(store
        .task_events_of(task, "closed", 1)
        .map_err(std::io::Error::other)?
        .into_iter()
        .next()
        .map(|e| e.body)
        .unwrap_or_default())
}

/// The spaces and tabs the owner keeps from qass: left out of the board when qass is the one
/// reading it (see [`crate::qass::called_by_qass`]), whatever they hold.
#[derive(Default)]
struct Hidden {
    spaces: BTreeSet<i64>,
    tabs: BTreeSet<i64>,
}

fn read(
    store: &Store,
    (live, qass_checks): (&HashMap<i64, AgentRow>, bool),
    hidden: &Hidden,
) -> std::io::Result<Board> {
    let now = now();
    let stored: HashMap<String, String> = store
        .session_titles()
        .map_err(std::io::Error::other)?
        .into_iter()
        .collect();
    let mut tasks = store.open_tasks().map_err(std::io::Error::other)?;
    let workspaces = store.workspaces().map_err(std::io::Error::other)?;
    let mut rows = HashMap::new();
    for w in &workspaces {
        for t in store.open_terminals(w.id).map_err(std::io::Error::other)? {
            rows.insert(t.id, t);
        }
    }
    // The name every reader gives an open terminal's agent (`agent_name`); a closed agent on
    // an item keeps the name it was linked under (`item_info`), any other its number.
    let name_of = |terminal: i64| {
        let row = rows.get(&terminal)?;
        let session = row.agent_session.as_deref();
        Some(agent_name(
            terminal,
            live.get(&terminal).and_then(|a| a.agent_title.clone()),
            session.and_then(|s| stored.get(s).cloned()),
            row.title.clone(),
            session,
        ))
    };
    // What a tab is doing: the daemon's live row, or the sleep the store kept for it.
    let asleep: BTreeMap<i64, i64> = store
        .asleep()
        .map_err(std::io::Error::other)?
        .into_iter()
        .map(|(terminal, _, since)| (terminal, since))
        .collect();
    // A claim is checked only where qass can check now and does not leave the tab alone.
    let tab = |terminal: i64| {
        let row = live.get(&terminal);
        tab_of(
            row.and_then(Live::of_row),
            &asleep,
            terminal,
            qass_checks && !row.is_some_and(|r| r.qass_skip),
        )
    };
    let waiting = store.waiting_since().map_err(std::io::Error::other)?;
    let items: Vec<ItemInfo> = store
        .items(now)
        .map_err(std::io::Error::other)?
        .into_iter()
        .map(|row| item_info(row, name_of, tab))
        .filter(|item| !item.workspace.is_some_and(|w| hidden.spaces.contains(&w)))
        .map(|mut item| {
            item.agents
                .retain(|a| !a.terminal.is_some_and(|t| hidden.tabs.contains(&t)));
            item
        })
        .collect();
    let mut spaces = Vec::new();
    let mut names = HashMap::new();
    for w in workspaces {
        names.insert(w.id, w.name.clone());
        if w.name == QASS_SPACE || hidden.spaces.contains(&w.id) {
            continue;
        }
        let mine: Vec<TaskRow>;
        (mine, tasks) = tasks.into_iter().partition(|t| t.workspace == Some(w.id));
        let mine = mine
            .into_iter()
            .filter(|t| !t.terminal.is_some_and(|t| hidden.tabs.contains(&t)));
        let mut open = Vec::new();
        for task in mine {
            let name = task.terminal.and_then(name_of).unwrap_or_else(|| {
                task.terminal
                    .map_or("no terminal".to_string(), |t| format!("terminal {t}"))
            });
            let newest = |kind: &str, n: usize| {
                store
                    .task_events_of(task.id, kind, n)
                    .map_err(std::io::Error::other)
            };
            let staged = newest("stage", 1)?.first().map_or(task.opened_at, |e| e.at);
            let plan = newest("plan", 1)?.into_iter().next().map(|e| e.body);
            let evidence = newest("evidence", EVIDENCE_SHOWN)?
                .into_iter()
                .map(|e| (e.at, e.body))
                .collect();
            let claim = match task.claimed_at {
                Some(_) => newest("done", 1)?.into_iter().next().map(|e| e.body),
                None => None,
            };
            let tab = task.terminal.map(tab).unwrap_or_default();
            let (state, status_at) = task_status(&task, waiting.get(&task.id).copied(), tab);
            open.push(Open {
                task,
                name,
                status: state,
                status_at,
                staged,
                plan,
                evidence,
                claim,
            });
        }
        spaces.push(Space {
            id: w.id,
            name: w.name,
            goal: w.goal,
            tasks: open,
        });
    }
    let mut closed = Vec::new();
    for task in store
        .closed_tasks_since(now - CLOSED_SECS, CLOSED_MAX)
        .map_err(std::io::Error::other)?
    {
        let space = task.workspace.and_then(|w| names.get(&w)).cloned();
        if space.as_deref() == Some(QASS_SPACE)
            || task.workspace.is_some_and(|w| hidden.spaces.contains(&w))
            || task.terminal.is_some_and(|t| hidden.tabs.contains(&t))
        {
            continue;
        }
        let note = closing_note(store, task.id)?;
        closed.push(Closed {
            task,
            space: space.unwrap_or_else(|| "a closed space".to_string()),
            note,
        });
    }
    let mut notes = HashMap::new();
    for i in items.iter().filter(|i| i.state == "done") {
        notes.insert(i.id, closing_note(store, i.id)?);
    }
    Ok(Board {
        asof: local_now(),
        now,
        items,
        notes,
        spaces,
        closed,
    })
}

const HEADER: &str = "Written by `qoop task board`; edits are overwritten.";

/// Text for a table cell: one line, its pipes escaped so the row keeps its columns.
fn cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

fn index(b: &Board) -> String {
    let mut out = format!("# Board\n\nAs of {}. {HEADER}\n\n## Items\n\n", b.asof);
    if b.items.is_empty() {
        out.push_str("None. `qoop task add --space <space> <title>` adds one.\n");
    } else {
        out.push_str("| Key | Title | State | Steps | Assignees |\n|---|---|---|---|---|\n");
        for i in &b.items {
            // A closed item's agents are named only: a stage there is stale.
            let who: Vec<String> = if i.state == "done" {
                i.agents.iter().map(|a| a.name.clone()).collect()
            } else {
                i.agents
                    .iter()
                    .filter(|a| !a.closed)
                    .map(|a| format!("{} ({}, {})", a.name, a.machine, a.stage))
                    .collect()
            };
            let state = i
                .outcome
                .as_deref()
                .map_or(i.state.clone(), |o| format!("{}, {o}", i.state));
            // Put away by the owner: off every board he sees, still done. Its plan, when one is
            // linked, read where the state is.
            let hidden = if i.hidden { ", hidden" } else { "" };
            let hidden = match (i.plan.as_str(), i.plan_by.as_deref()) {
                ("", _) => hidden.to_string(),
                (plan, Some(by)) => format!("{hidden}, plan {plan} by {by}"),
                (plan, None) => format!("{hidden}, plan {plan}"),
            };
            let _ = writeln!(
                out,
                "| {} | {} | {state}{hidden} | {}/{} | {} |",
                i.key,
                cell(&i.title),
                i.steps_done,
                i.steps_total,
                if who.is_empty() {
                    "none".to_string()
                } else {
                    cell(&who.join(", "))
                }
            );
        }
    }
    out.push_str("\n## Spaces\n\n");
    if b.spaces.is_empty() {
        out.push_str("No spaces.\n");
    } else {
        out.push_str("| Space | Goal | Open tasks | Claimed done |\n|---|---|---|---|\n");
        for s in &b.spaces {
            let claimed = s
                .tasks
                .iter()
                .filter(|t| t.task.claimed_at.is_some())
                .count();
            let _ = writeln!(
                out,
                "| [{}](space-{}.md) | {} | {} | {} |",
                cell(&s.name).replace(']', "\\]"),
                s.id,
                cell(s.goal.as_deref().unwrap_or("none set")),
                s.tasks.len(),
                claimed
            );
        }
    }
    out.push_str("\n## Closed in the last 24 h\n\n");
    // Closed items first, newest first; an agent task of a listed item is that same job.
    let mut items: Vec<&ItemInfo> = b
        .items
        .iter()
        .filter(|i| i.state == "done" && !i.hidden)
        .filter(|i| i.closed_at.is_some_and(|at| at >= b.now - CLOSED_SECS))
        .collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.closed_at));
    items.truncate(CLOSED_MAX);
    let listed: Vec<i64> = items
        .iter()
        .flat_map(|i| i.agents.iter().map(|a| a.task))
        .collect();
    let tasks: Vec<&Closed> = b
        .closed
        .iter()
        .filter(|c| !listed.contains(&c.task.id))
        .collect();
    if items.is_empty() && tasks.is_empty() {
        out.push_str("None.\n");
    }
    // The exact close time rides along unseen, for a reader comparing it with its own clock.
    for i in &items {
        let _ = writeln!(
            out,
            "- {} ({}): {}. Closed {}: {} <!-- closed_at {} -->",
            i.outcome.as_deref().unwrap_or("closed"),
            i.key,
            cell(&i.title),
            ago(i.closed_at, b.now),
            one_line(b.notes.get(&i.id).map_or("", String::as_str)),
            i.closed_at.unwrap_or_default()
        );
    }
    for c in tasks {
        let t = &c.task;
        let _ = writeln!(
            out,
            "- {} ({}, task #{}): {}. Closed {}: {} <!-- closed_at {} -->",
            t.outcome.as_deref().unwrap_or("closed"),
            c.space,
            t.id,
            t.goal,
            ago(t.closed_at, b.now),
            one_line(&c.note),
            t.closed_at.unwrap_or_default()
        );
    }
    out
}

fn space(b: &Board, s: &Space) -> String {
    let mut out = format!(
        "# {} (space {})\n\nAs of {}. {HEADER}\n\n",
        one_line(&s.name),
        s.id,
        b.asof
    );
    match &s.goal {
        Some(goal) => {
            let _ = writeln!(out, "Goal: {goal}");
        }
        None => {
            let _ = writeln!(
                out,
                "Goal: none set (`qoop space goal {} \"<what the owner wants>\"`)",
                s.id
            );
        }
    }
    if s.tasks.is_empty() {
        out.push_str("\nNo open task.\n");
    }
    for o in &s.tasks {
        let t = &o.task;
        // What its agent is doing, as the board page words it (`asleep · 13 d`); a status with no
        // word of its own leaves the stage to say it.
        let doing = crate::board_page::status_said(o.status.as_str(), o.status_at, b.now)
            .map_or(String::new(), |(word, _)| format!(" · {word}"));
        let _ = writeln!(
            out,
            "\n## {} · task #{} · {} since {}{doing} · updated {}\n\nGoal: {}",
            one_line(&o.name),
            t.id,
            t.stage,
            ago(Some(o.staged), b.now),
            ago(Some(t.updated_at), b.now),
            t.goal
        );
        if let (Some(at), Some(summary)) = (t.claimed_at, &o.claim) {
            let _ = writeln!(
                out,
                "\nClaimed done {}: {}",
                ago(Some(at), b.now),
                one_line(summary)
            );
        }
        if let Some(plan) = &o.plan {
            out.push_str("\nPlan:\n\n");
            for line in plan.lines() {
                let _ = writeln!(out, "> {line}");
            }
        }
        if !o.evidence.is_empty() {
            out.push_str("\nEvidence, newest first:\n\n");
            for (at, body) in &o.evidence {
                let _ = writeln!(out, "- {}: {}", ago(Some(*at), b.now), one_line(body));
            }
        }
    }
    out
}

/// Write `text` to `path` whole or not at all: a reader never sees half a file.
fn put(dir: &Path, name: &str, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(text.as_bytes())?;
    file.persist(dir.join(name)).map_err(|e| e.error)?;
    Ok(())
}

fn write_dir(b: &Board, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let keep: Vec<String> = b
        .spaces
        .iter()
        .map(|s| format!("space-{}.md", s.id))
        .collect();
    for s in &b.spaces {
        put(dir, &format!("space-{}.md", s.id), &space(b, s))?;
    }
    put(dir, "README.md", &index(b))?;
    // Only this command's own files of spaces that are gone; nothing else in the folder.
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        let ours = name
            .strip_prefix("space-")
            .and_then(|n| n.strip_suffix(".md"))
            .is_some_and(|n| n.parse::<i64>().is_ok());
        if ours && !keep.contains(&name) {
            std::fs::remove_file(dir.join(&name))?;
        }
    }
    Ok(())
}

/// What qass is not shown: the owner's skips when qass is the reader, nothing for anyone else.
fn hidden_from_qass(store: &Store) -> std::io::Result<Hidden> {
    if !crate::qass::may_be_qass() {
        return Ok(Hidden::default());
    }
    let (spaces, tabs) = store.qass_skips().map_err(std::io::Error::other)?;
    if (spaces.is_empty() && tabs.is_empty()) || !crate::qass::called_by_qass() {
        return Ok(Hidden::default());
    }
    Ok(Hidden { spaces, tabs })
}

/// `qoop task board [--json] [--dir DIR]`.
pub(crate) fn run(dir: Option<&Path>, json: bool) -> std::io::Result<()> {
    let runtime = paths::runtime_dir().map_err(std::io::Error::other)?;
    let Some(store) = crate::existing_store(&runtime)? else {
        if json {
            println!("[]");
        } else {
            println!("no spaces yet. `qoop` starts one.");
        }
        return Ok(());
    };
    let (live, qass_checks) = live_rows();
    let board = read(&store, (&live, qass_checks), &hidden_from_qass(&store)?)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&board.items).map_err(std::io::Error::other)?
        );
        return Ok(());
    }
    match dir {
        Some(dir) => write_dir(&board, dir),
        None => {
            print!("{}", index(&board));
            for s in &board.spaces {
                print!("\n{}", space(&board, s));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> (tempfile::TempDir, Board) {
        let mut store = Store::in_memory().expect("store");
        let ws = store.create_workspace("web | shop", 1).unwrap();
        let qass = store.create_workspace(QASS_SPACE, 1).unwrap();
        let t = store.create_terminal(5, ws, "/w", "/bin/sh", 2).unwrap();
        let q = store.create_terminal(6, qass, "/q", "/bin/sh", 2).unwrap();
        store
            .set_space_goal(ws, "checkout works on the phone", 3)
            .unwrap();
        let task = store.open_task(t, "fix login", false, 4).unwrap();
        store
            .set_task_goal(task, "login works; done when e2e passes", 5)
            .unwrap();
        store.task_plan(task, "1. read\n2. fix", 6).unwrap();
        for n in 0..5 {
            store
                .task_evidence(task, &format!("check {n}: ok"), 7 + n)
                .unwrap();
        }
        store.claim_task_done(task, "fixed, e2e green", 20).unwrap();
        store.open_task(q, "qass notes", false, 4).unwrap();
        let done = store.create_terminal(7, ws, "/w", "/bin/sh", 2).unwrap();
        let old = store.open_task(done, "old work", false, 4).unwrap();
        store
            .close_task(
                old,
                qoop_store::Outcome::Verified,
                "PR #9 merged, e2e ran",
                now(),
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut b = read(&store, (&HashMap::new(), false), &Hidden::default()).unwrap();
        b.asof = "2026-10-10 21:40".into();
        (dir, b)
    }

    /// An open task's heading says what its agent is doing and since when, in the board page's
    /// words: a slept tab, a live one and a claim, each from the same status.
    #[test]
    fn an_open_tasks_line_says_its_status_and_its_age() {
        let mut store = Store::in_memory().expect("store");
        let ws = store.create_workspace("web", 1).unwrap();
        let now = now();
        let day = 86_400;
        let mut tabs = Vec::new();
        for n in 5..8 {
            let t = store.create_terminal(n, ws, "/w", "/bin/sh", 2).unwrap();
            let task = store
                .open_task(t, &format!("job {n}"), false, now - 30 * day)
                .unwrap();
            tabs.push((t, task));
        }
        store
            .sleep_terminals(
                now - 13 * day,
                &[qoop_store::Checkpoint {
                    terminal: tabs[0].0,
                    agent: None,
                    agent_session: None,
                }],
            )
            .unwrap();
        let busy: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": 6, "workspace": ws, "state": "working", "readiness": "ready",
            "agent": true, "last_active_at_unix": now - 3 * day,
        }))
        .unwrap();
        store
            .claim_task_done(tabs[2].1, "ok", now - 8 * day)
            .unwrap();
        let claimer = |skip: bool| -> AgentRow {
            serde_json::from_value(serde_json::json!({
                "terminal": 7, "workspace": ws, "state": "idle", "readiness": "ready",
                "agent": true, "qass_skip": skip,
            }))
            .unwrap()
        };
        let live = HashMap::from([(6, busy.clone()), (7, claimer(false))]);
        let b = read(&store, (&live, true), &Hidden::default()).unwrap();
        let text = space(&b, &b.spaces[0]);
        for want in [
            "task #1 · investigating since 30 d ago · asleep · 13 d · updated",
            "task #2 · investigating since 30 d ago · working · 3 d · updated",
            "task #3 · investigating since 30 d ago · qass checks · 8 d · updated",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        // A claim nobody can check waits on him: qass cannot check now, or it leaves the tab
        // alone. The snapshot rides the daemon's agents reply.
        let skipped = HashMap::from([(6, busy.clone()), (7, claimer(true))]);
        for (live, checks) in [(&live, false), (&skipped, true)] {
            let b = read(&store, (live, checks), &Hidden::default()).unwrap();
            let text = space(&b, &b.spaces[0]);
            let want = "task #3 · investigating since 30 d ago · waits on you · 8 d";
            assert!(text.contains(want), "{want}: {text}");
        }
        // A claim whose tab is gone is qass's to close failed at its Start, watcher or not.
        let gone = HashMap::from([(6, busy)]);
        for checks in [true, false] {
            let b = read(&store, (&gone, checks), &Hidden::default()).unwrap();
            let text = space(&b, &b.spaces[0]);
            let want = "task #3 · investigating since 30 d ago · qass checks · 8 d";
            assert!(text.contains(want), "{want}: {text}");
        }
    }

    /// A closed item is listed among the closed with its close time, and the agent task that
    /// did it is not listed again; its row names its agent with no stage.
    #[test]
    fn closed_items_are_listed_once_and_say_no_stage() {
        let mut store = Store::in_memory().expect("store");
        let ws = store.create_workspace("web", 1).unwrap();
        let t = store.create_terminal(5, ws, "/w", "/bin/sh", 2).unwrap();
        let (item, key) = store.add_item(ws, "checkout", "", None, 2).unwrap();
        let task = store.open_task(t, "fix checkout", false, 3).unwrap();
        store.link_task(task, item, 4).unwrap();
        store.set_task_stage(task, "waiting-human", 5).unwrap();
        let second = store.create_terminal(8, ws, "/w", "/bin/sh", 2).unwrap();
        let did = store.open_task(second, "wire checkout", false, 3).unwrap();
        store.link_task(did, item, 4).unwrap();
        store
            .close_task(did, qoop_store::Outcome::Verified, "PR merged", now())
            .unwrap();
        store
            .close_task(item, qoop_store::Outcome::Verified, "checked", now())
            .unwrap();
        let other = store.create_terminal(6, ws, "/w", "/bin/sh", 2).unwrap();
        let alone = store.open_task(other, "side job", false, 3).unwrap();
        store
            .close_task(alone, qoop_store::Outcome::Verified, "ran", now())
            .unwrap();
        let b = read(&store, (&HashMap::new(), false), &Hidden::default()).unwrap();
        let readme = index(&b);
        assert!(
            readme.contains(&format!(
                "- verified ({key}): checkout. Closed just now: verified: checked"
            )),
            "{readme}"
        );
        assert!(readme.contains("side job"), "{readme}");
        assert!(
            !readme.contains("wire checkout"),
            "one job, one line: {readme}"
        );
        assert!(!readme.contains("waiting-human"), "{readme}");
        assert!(!readme.contains("None."), "{readme}");
    }

    /// A board read for qass leaves out what the owner keeps from it: a skipped space has no row
    /// and no file, a skipped tab's task is not listed. The owner's own board shows all of it.
    #[test]
    fn a_board_read_for_qass_leaves_out_what_the_owner_keeps_from_it() {
        let mut store = Store::in_memory().expect("store");
        let shown = store.create_workspace("shown", 1).unwrap();
        let kept = store.create_workspace("kept", 1).unwrap();
        let plain = store.create_terminal(5, shown, "/w", "/bin/sh", 2).unwrap();
        let private = store.create_terminal(6, shown, "/w", "/bin/sh", 2).unwrap();
        let inside = store.create_terminal(7, kept, "/w", "/bin/sh", 2).unwrap();
        store.open_task(plain, "plain work", false, 3).unwrap();
        store.open_task(private, "private work", false, 3).unwrap();
        store
            .open_task(inside, "kept space work", false, 3)
            .unwrap();
        let text = |hidden: &Hidden| {
            let dir = tempfile::tempdir().unwrap();
            write_dir(
                &read(&store, (&HashMap::new(), false), hidden).unwrap(),
                dir.path(),
            )
            .unwrap();
            let mut all = String::new();
            for entry in std::fs::read_dir(dir.path()).unwrap() {
                all += &std::fs::read_to_string(entry.unwrap().path()).unwrap();
            }
            all
        };
        let owner = text(&Hidden::default());
        for word in ["plain work", "private work", "kept space work", "kept"] {
            assert!(owner.contains(word), "{word}: {owner}");
        }
        let qass = text(&Hidden {
            spaces: BTreeSet::from([kept]),
            tabs: BTreeSet::from([private]),
        });
        assert!(qass.contains("plain work"), "{qass}");
        for word in ["private work", "kept space work", "kept"] {
            assert!(!qass.contains(word), "{word}: {qass}");
        }
    }

    /// The folder holds the owner's goal and each open task's goal, stage, plan, newest
    /// evidence and claim; qass's own space is left out; a gone space's file is removed and
    /// nothing else is touched.
    #[test]
    fn the_board_shows_goals_and_work_and_cleans_only_its_own_files() {
        let (dir, b) = board();
        std::fs::write(dir.path().join("space-999.md"), "gone").unwrap();
        std::fs::write(dir.path().join("notes.md"), "mine").unwrap();
        write_dir(&b, dir.path()).unwrap();
        let readme = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
        assert!(readme.contains("As of 2026-10-10 21:40."), "{readme}");
        assert!(
            readme.contains(r"| [web \| shop](space-1.md) | checkout works on the phone | 1 | 1 |"),
            "{readme}"
        );
        assert!(
            readme.contains(
                "verified (web | shop, task #3): old work. Closed just now: verified: PR #9 merged, e2e ran <!-- closed_at "
            ),
            "{readme}"
        );
        assert!(!readme.contains(QASS_SPACE), "{readme}");
        let space = std::fs::read_to_string(dir.path().join("space-1.md")).unwrap();
        assert!(
            space.contains("Goal: checkout works on the phone"),
            "{space}"
        );
        assert!(
            space.contains("Goal: login works; done when e2e passes"),
            "{space}"
        );
        assert!(space.contains("Claimed done"), "{space}");
        assert!(space.contains("task #1 · investigating since "), "{space}");
        assert!(space.contains("> 2. fix"), "{space}");
        assert!(
            space.contains("check 4: ok") && !space.contains("check 1: ok"),
            "{space}"
        );
        assert!(!dir.path().join("space-999.md").exists());
        assert!(
            !dir.path().join("space-2.md").exists(),
            "qass's space has no file"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "mine"
        );
    }
}
