//! `qoop task board`: the board's items, then every space with the owner's goal for it and
//! the open tasks in it, as Markdown a person or qass can read; `--json`, the items alone.
//!
//! Read from the record itself, as `qoop ls` is: no frame to fit in, no events cut, nothing
//! held open on the daemon. Only the agents' live names come from the daemon, through an
//! observer connection, and a daemon that is not there leaves the stored names. With `--dir`
//! the same text is a folder: `README.md` and one `space-<id>.md` per space (ids, because two
//! spaces may share a name), rewritten whole each time, so a copy never drifts from the
//! record; it only ages, and says how old it is.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::qass_page::ago;

use qoop_runtime::control::{ControlRequest, ControlResponse, ItemInfo, agent_name, item_info};
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

/// The live sidebar names of this machine's agents, by terminal; empty without a daemon.
fn live_titles() -> HashMap<i64, String> {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return HashMap::new();
    };
    rt.block_on(async {
        let Ok(dir) = paths::runtime_dir() else {
            return HashMap::new();
        };
        let Ok((stream, inbuf)) =
            client::connect_observer(&paths::socket_path(&dir), "qoop-board").await
        else {
            return HashMap::new();
        };
        let request = ControlRequest::ListAgents {
            id: crate::request_id("board"),
        };
        match crate::ask_over(stream, inbuf, &request, crate::ASK_PATIENCE).await {
            Ok(ControlResponse::Agents { agents, .. }) => agents
                .into_iter()
                .filter_map(|a| Some((i64::from(a.terminal), a.agent_title?)))
                .collect(),
            _ => HashMap::new(),
        }
    })
}

fn read(store: &Store, live: &HashMap<i64, String>) -> std::io::Result<Board> {
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
            live.get(&terminal).cloned(),
            session.and_then(|s| stored.get(s).cloned()),
            row.title.clone(),
            session,
        ))
    };
    let items = store
        .items(now)
        .map_err(std::io::Error::other)?
        .into_iter()
        .map(|row| item_info(row, name_of))
        .collect();
    let mut spaces = Vec::new();
    let mut names = HashMap::new();
    for w in workspaces {
        names.insert(w.id, w.name.clone());
        if w.name == QASS_SPACE {
            continue;
        }
        let mine: Vec<TaskRow>;
        (mine, tasks) = tasks.into_iter().partition(|t| t.workspace == Some(w.id));
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
            open.push(Open {
                task,
                name,
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
        if space.as_deref() == Some(QASS_SPACE) {
            continue;
        }
        let note = store
            .task_events_of(task.id, "closed", 1)
            .map_err(std::io::Error::other)?
            .into_iter()
            .next()
            .map(|e| e.body)
            .unwrap_or_default();
        closed.push(Closed {
            task,
            space: space.unwrap_or_else(|| "a closed space".to_string()),
            note,
        });
    }
    Ok(Board {
        asof: local_now(),
        now,
        items,
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
            let who: Vec<String> = i
                .agents
                .iter()
                .filter(|a| !a.closed)
                .map(|a| format!("{} ({}, {})", a.name, a.machine, a.stage))
                .collect();
            let _ = writeln!(
                out,
                "| {} | {} | {} | {}/{} | {} |",
                i.key,
                cell(&i.title),
                i.outcome
                    .as_deref()
                    .map_or(i.state.clone(), |o| format!("{}, {o}", i.state)),
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
    if b.closed.is_empty() {
        out.push_str("None.\n");
    }
    // The exact close time rides along unseen, for a reader comparing it with its own clock.
    for c in &b.closed {
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
        let _ = writeln!(
            out,
            "\n## {} · task #{} · {} since {} · updated {}\n\nGoal: {}",
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
    let board = read(&store, &live_titles())?;
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
        let mut b = read(&store, &HashMap::new()).unwrap();
        b.asof = "2026-10-10 21:40".into();
        (dir, b)
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
