//! The daemon's side of `qoop task`: which task a request means, the write, and an answer
//! that fits one frame.
//!
//! A terminal names its own task through its token, checked like a hook report: a token
//! that does not match refuses the call, with no guessing. A task named by id needs no
//! terminal, because the socket is the owner's own. Who closes a task is a convention (qass
//! or the person), not a check.

use std::collections::BTreeMap;

use qoop_domain::{CapabilityToken, TerminalId};
use qoop_store::{Outcome, TaskEventRow, TaskRow};

use crate::agents::AgentRegistry;
use crate::control::{
    Actor, ControlResponse, ItemCardInfo, ItemEventInfo, StepInfo, TASK_REF_GOAL, Tab,
    TaskEventInfo, TaskInfo, TaskOp, TaskOutcome, TaskRef, item_info, tab_of, task_status,
};
use crate::store::{HandleError, StoreHandle, now};

/// The space qass works in: nothing there opens a task on its own.
const QASS_SPACE: &str = "qass";

/// The most a `Tasks` answer carries, serialized, under the 65,536 byte frame.
const ANSWER_BYTES: usize = 60_000;

/// The most events one read looks at before the byte cap decides.
const EVENTS_READ: usize = 200;

/// Whether `token` is this terminal's, the same check a hook report passes.
pub(crate) fn admitted(agents: &AgentRegistry, terminal: u32, token: &str) -> bool {
    let id = TerminalId::new(terminal.to_string());
    let token = CapabilityToken::new(token.to_string());
    let Some(generation) = agents.agent(terminal).map(|a| a.belief.generation) else {
        return false;
    };
    agents
        .admit(terminal, &id, Some(&token), generation)
        .is_ok()
}

fn failed(id: String, reason: impl Into<String>) -> ControlResponse {
    ControlResponse::Failed {
        id,
        reason: reason.into(),
    }
}

/// A write that landed on `task`.
fn written(id: String, task: i64) -> ControlResponse {
    ControlResponse::TaskWritten {
        id,
        task,
        key: None,
    }
}

fn store_failed(id: String, error: HandleError) -> ControlResponse {
    match error {
        // The deadline does not cancel the write, so it may still land.
        HandleError::Timeout => failed(
            id,
            "qoop did not confirm in time; it may have been recorded, check qoop task show",
        ),
        HandleError::Store(e) => failed(id, e.to_string()),
    }
}

/// The terminal a request speaks for, once its token is checked. `None` when it named none.
fn speaker(
    agents: &AgentRegistry,
    terminal: Option<u32>,
    token: Option<&str>,
) -> Result<Option<u32>, String> {
    match (terminal, token) {
        (None, _) => Ok(None),
        (Some(t), Some(token)) if admitted(agents, t, token) => Ok(Some(t)),
        (Some(t), _) => Err(format!("the task request for terminal {t} was refused")),
    }
}

/// How long a done claim waits for its record: the session found within the first, git within
/// the second. Their sum stays well under the 10 s a `qoop task` waits for its answer.
const RECORD_LOCATE: std::time::Duration = std::time::Duration::from_secs(1);
const RECORD_GIT: std::time::Duration = std::time::Duration::from_secs(3);

/// What a done claim's `record` event holds, as compact JSON: the claiming `terminal`, and
/// `"skipped": true` when the owner had asked qass to leave that terminal or its space alone
/// (`qoop messages --task` then keeps qass off it, after the tab is gone), the agent's
/// `program` and its `session` id (what `qoop messages --task` finds the session by again),
/// where the session was then (`source`, a `qoop_parsed::Source`, kept for the reader's
/// information), and its checkout's `head` and whether that was `clean`. Each is left out
/// when it is not known, never guessed, and finding them never fails the claim.
pub(crate) async fn record(
    dirs: Option<qoop_parsed::Dirs>,
    terminal: Option<u32>,
    skipped: bool,
    program: Option<String>,
    session: Option<String>,
    cwd: Option<String>,
) -> String {
    let source = match (dirs, program.clone(), session.clone()) {
        (Some(dirs), Some(program), Some(session)) => {
            crate::blocking::chat(RECORD_LOCATE, move || {
                qoop_parsed::locate_session(&program, &session, &dirs)
            })
            .await
            .ok()
            .flatten()
        }
        _ => None,
    };
    let (head, clean) = match cwd {
        Some(cwd) => crate::blocking::probe_asked(
            &cwd.clone(),
            RECORD_GIT + std::time::Duration::from_millis(500),
            move || crate::changes::head_and_clean(&cwd, RECORD_GIT),
        )
        .await
        .unwrap_or((None, None)),
        None => (None, None),
    };
    let mut record = serde_json::Map::new();
    let mut put = |key: &str, value: Option<serde_json::Value>| {
        if let Some(value) = value {
            record.insert(key.to_string(), value);
        }
    };
    put("terminal", terminal.map(Into::into));
    put("skipped", skipped.then_some(true.into()));
    put("program", program.map(Into::into));
    put("session", session.map(Into::into));
    put("source", source.and_then(|s| serde_json::to_value(s).ok()));
    put("head", head.map(Into::into));
    put("clean", clean.map(Into::into));
    serde_json::Value::Object(record).to_string()
}

/// Who asked for a close, as its event records it: `qass` for a terminal in qass's own space,
/// `T<n>/<generation>` for any other whose token matches, else `unknown`. Never the owner by
/// default: a caller with no identity is unknown.
fn actor_label(agents: &AgentRegistry, actor: Option<&Actor>, space: Option<&str>) -> String {
    let Some(actor) = actor.filter(|a| admitted(agents, a.terminal, &a.token)) else {
        return "unknown".into();
    };
    if space == Some(QASS_SPACE) {
        return "qass".into();
    }
    match agents.agent(actor.terminal) {
        Some(agent) => format!("T{}/{}", actor.terminal, agent.belief.generation),
        None => "unknown".into(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write(
    id: String,
    terminal: Option<u32>,
    token: Option<String>,
    task: Option<i64>,
    action: TaskOp,
    space: Option<String>,
    name: Option<String>,
    record: Option<String>,
    agents: &AgentRegistry,
    store: &StoreHandle,
) -> ControlResponse {
    let speaker = match speaker(agents, terminal, token.as_deref()) {
        Ok(s) => s,
        Err(reason) => return failed(id, reason),
    };
    let now = now();
    // An item belongs to a space, not a terminal: its own branch, before any target.
    if let TaskOp::Add {
        workspace,
        goal,
        description,
    } = action
    {
        // An agent's own find waits for the owner's start: a coding agent in a tab outside
        // qass's space. qass adds what he asked for, and a person in a shell tab is him.
        let agent_tab = speaker.filter(|&t| {
            agents.agent(t).is_some_and(|a| {
                matches!(
                    a.identity,
                    qoop_domain::AgentIdentity::Known { agent: true, .. }
                )
            })
        });
        return match store
            .run(move |s| {
                let spaces = s.workspaces()?;
                let space_of = |id: i64| spaces.iter().find(|w| w.id == id).map(|w| w.name.clone());
                let here = match speaker {
                    Some(t) => s.terminal_workspace(i64::from(t))?,
                    None => None,
                };
                let Some(workspace) = workspace.or(here) else {
                    return Ok(Err(
                        "no space named, and not inside a qoop terminal: add --space <space>",
                    ));
                };
                if space_of(workspace).as_deref() == Some(QASS_SPACE) {
                    return Ok(Err(
                        "qass's own space has no board: add the item to the space the work is in",
                    ));
                }
                // An agent's own find waits for the owner; qass adds what he asked for.
                let by = agent_tab
                    .filter(|_| here.and_then(space_of).as_deref() != Some(QASS_SPACE))
                    .map(|t| format!("T{t}"));
                let description = description.unwrap_or_default();
                s.add_item(workspace, &goal, &description, by.as_deref(), now)
                    .map(Ok)
            })
            .await
        {
            Ok(Ok((task, key))) => {
                agents.tasks_changed();
                ControlResponse::TaskWritten {
                    id,
                    task,
                    key: Some(key),
                }
            }
            Ok(Err(reason)) => failed(id, reason),
            Err(e) => store_failed(id, e),
        };
    }
    // An agent on another machine is named by its item and its remote, never by a task here.
    if let TaskOp::LinkRemote {
        item,
        remote,
        task: theirs,
        name,
        stage,
    } = action
    {
        return match store
            .run(move |s| s.link_remote(item, &remote, theirs, &name, &stage, now))
            .await
        {
            Ok(changed) => {
                if changed {
                    agents.tasks_changed();
                }
                written(id, item)
            }
            Err(e) => store_failed(id, e),
        };
    }
    // What qass starts is marked as qass's; a start from anywhere else writes nothing.
    if let TaskOp::Spawned { terminal: started } = action {
        if speaker.is_none() || space.as_deref() != Some(QASS_SPACE) {
            return written(id, 0);
        }
        return match store
            .run(move |s| s.mark_spawned(i64::from(started), QASS_SPACE))
            .await
        {
            Ok(_) => written(id, 0),
            Err(e) => store_failed(id, e),
        };
    }
    // A goal or a link from a terminal lands on its live task: begun first, exactly as a
    // hook's Begin would, so one set before the first edit, or after a done claim and a new
    // request, is never written onto the old task.
    let goal_here = matches!(action, TaskOp::Goal { .. }) && task.is_none();
    let link_here = matches!(action, TaskOp::Link { .. }) && task.is_none();
    if matches!(action, TaskOp::Begin) || goal_here || link_here {
        let Some(terminal) = speaker else {
            return failed(
                id,
                if goal_here || link_here {
                    "no task named, and not inside a qoop terminal"
                } else {
                    "a task is begun from inside a qoop terminal"
                },
            );
        };
        // qass edits its own files; it never has a task of its own.
        // An unread name could be qass's: nothing is opened on a guess, the next edit asks again.
        if space.is_none() || space.as_deref() == Some(QASS_SPACE) {
            if link_here {
                return failed(
                    id,
                    if space.is_none() {
                        "qoop could not read this terminal's space in time; nothing was linked, \
                         try again"
                    } else {
                        "qass's own space keeps no task, so there is nothing to link"
                    },
                );
            }
            return written(id, 0);
        }
        let (goal, asked_at) = match agents.last_prompt(terminal) {
            Some((goal, at)) => (goal, Some(at)),
            None => (String::new(), None),
        };
        let (wanted, item) = match action {
            TaskOp::Goal { goal } => (Some(goal), None),
            TaskOp::Link { item } => (None, Some(item)),
            _ => (None, None),
        };
        return match store
            .run(move |s| {
                let wanted = wanted
                    .map(|g| qoop_store::task::goal_text(&g))
                    .transpose()?;
                // A link opens its task named after the item, refused before anything is
                // begun when the item is not open; an edit's, after the last request.
                let (task, opened) = match item {
                    Some(item) => s.begin_linked(i64::from(terminal), item, asked_at, now)?,
                    None => s.begin_task(i64::from(terminal), &goal, asked_at, now)?,
                };
                let changed = match &wanted {
                    Some(wanted) => s.set_task_goal(task, wanted, now)?,
                    None => false,
                };
                let linked = match item {
                    Some(item) => s.link_task(task, item, now)?,
                    None => false,
                };
                // What the agent is called now, kept for its card once its terminal is gone.
                if let (Some(_), Some(name)) = (item, &name) {
                    s.name_agent(task, name)?;
                }
                Ok((task, opened || changed, linked))
            })
            .await
        {
            Ok((task, shown, linked)) => {
                if shown {
                    agents.announce(terminal);
                }
                if shown || linked {
                    agents.tasks_changed();
                }
                written(id, task)
            }
            Err(e) => store_failed(id, e),
        };
    }
    if let TaskOp::New { goal, replace } = action {
        let Some(terminal) = speaker else {
            return failed(id, "a task is opened from inside a qoop terminal");
        };
        return match store
            .run(move |s| s.open_task(i64::from(terminal), &goal, replace, now))
            .await
        {
            Ok(task) => {
                agents.announce(terminal);
                agents.tasks_changed();
                written(id, task)
            }
            Err(e) => store_failed(id, e),
        };
    }
    if matches!(
        action,
        TaskOp::PlanFile { .. }
            | TaskOp::Approve { .. }
            | TaskOp::ShowMe { .. }
            | TaskOp::Next { .. }
    ) {
        // qass is a terminal in qass's own space, its token checked above.
        let qass = speaker.is_some() && space.as_deref() == Some(QASS_SPACE);
        return Box::pin(plan_write(
            id, speaker, qass, task, action, agents, store, now,
        ))
        .await;
    }
    if matches!(
        action,
        TaskOp::Describe { .. }
            | TaskOp::Steps { .. }
            | TaskOp::PhasedSteps { .. }
            | TaskOp::Check { .. }
            | TaskOp::Link { .. }
            | TaskOp::Unlink
            | TaskOp::Start
            | TaskOp::Reopen
            | TaskOp::Dismiss { .. }
    ) {
        return Box::pin(item_write(id, speaker, task, action, agents, store, now)).await;
    }
    if matches!(action, TaskOp::Close { .. }) && task.is_none() {
        return failed(id, "close names its task: qoop task close <id>");
    }
    // The task meant: the one named, which must be the speaker's own when a speaker came
    // with it, or the speaker's open one.
    let target = store
        .run(move |s| match (task, speaker) {
            (Some(task), None) => Ok(s.task(task)?.map(|t| (task, t.terminal))),
            (Some(task), Some(terminal)) => Ok(s
                .task(task)?
                .filter(|t| t.terminal == Some(i64::from(terminal)))
                .map(|t| (task, t.terminal))),
            (None, Some(terminal)) => Ok(s
                .open_task_of(i64::from(terminal))?
                .map(|t| (t.id, t.terminal))),
            (None, None) => Ok(None),
        })
        .await;
    // Who asks for a close, recorded on it and nothing more.
    let by = match &action {
        TaskOp::Close { actor, .. } => Some(actor_label(agents, actor.as_ref(), space.as_deref())),
        _ => None,
    };
    let (task, home) = match target {
        Ok(Some(found)) => found,
        Ok(None) => {
            return failed(
                id,
                match (task, speaker) {
                    (Some(task), Some(t)) => format!("task #{task} is not terminal {t}'s"),
                    (Some(task), None) => format!("no task #{task}"),
                    (None, Some(_)) => {
                        "this terminal has no open task: qoop task new <goal>".into()
                    }
                    (None, None) => "no task named, and not inside a qoop terminal".into(),
                },
            );
        }
        Err(e) => return store_failed(id, e),
    };
    let wrote = store
        .run(move |s| match action {
            TaskOp::New { .. }
            | TaskOp::Begin
            | TaskOp::Add { .. }
            | TaskOp::Describe { .. }
            | TaskOp::Steps { .. }
            | TaskOp::PhasedSteps { .. }
            | TaskOp::Check { .. }
            | TaskOp::Link { .. }
            | TaskOp::Unlink
            | TaskOp::Start
            | TaskOp::Reopen
            | TaskOp::Dismiss { .. }
            | TaskOp::PlanFile { .. }
            | TaskOp::Approve { .. }
            | TaskOp::ShowMe { .. }
            | TaskOp::Next { .. }
            | TaskOp::LinkRemote { .. }
            | TaskOp::Spawned { .. } => unreachable!("handled above"),
            TaskOp::Stage { stage } => s.set_task_stage(task, &stage, now),
            TaskOp::Goal { goal } => s.set_task_goal(task, &goal, now),
            TaskOp::Plan { text } => s.task_plan(task, &text, now).map(|()| false),
            TaskOp::Evidence { text } => s.task_evidence(task, &text, now).map(|()| false),
            TaskOp::Done { summary } => {
                s.claim_task_done(task, &summary, now)?;
                // After the claim and apart from it: a record that cannot be written never
                // fails the claim.
                if let Some(record) = &record {
                    let _ = s.task_record(task, record, now);
                }
                Ok(true)
            }
            TaskOp::Close {
                outcome, reason, ..
            } => {
                let outcome = match outcome {
                    TaskOutcome::Verified => Outcome::Verified,
                    TaskOutcome::Failed => Outcome::Failed,
                    // Items only: a task ends with its agent's work, it is not taken off a board.
                    TaskOutcome::Dropped => {
                        s.open_item_goal(task)?;
                        Outcome::Abandoned
                    }
                };
                // An agent's task that ended with its tab: name the item still to close.
                let flag = if outcome == Outcome::Failed {
                    "--failed"
                } else {
                    "--verified"
                };
                match s.close_task_by(task, outcome, by.as_deref(), &reason, now) {
                    Err(qoop_store::StoreError::Task(qoop_store::TaskError::Closed(t))) => {
                        match s.open_item_key(t)? {
                            Some(key) => {
                                Err(qoop_store::TaskError::ClosedOnItem(t, key, flag).into())
                            }
                            None => Err(qoop_store::TaskError::Closed(t).into()),
                        }
                    }
                    other => other.map(|_| true),
                }
            }
        })
        .await;
    match wrote {
        Ok(shown) => {
            // Only what a row shows: plan and evidence change nothing a reader draws.
            if shown && let Some(home) = home.and_then(|t| u32::try_from(t).ok()) {
                agents.announce(home);
            }
            if shown {
                agents.tasks_changed();
            }
            written(id, task)
        }
        Err(e) => store_failed(id, e),
    }
}

/// A write on an item. By id, the item named (for an unlink, an item or a task); from a
/// terminal, the item its open task is linked to, or for an unlink that task.
async fn item_write(
    id: String,
    speaker: Option<u32>,
    task: Option<i64>,
    action: TaskOp,
    agents: &AgentRegistry,
    store: &StoreHandle,
    now: i64,
) -> ControlResponse {
    if task.is_none() {
        if matches!(
            action,
            TaskOp::Start | TaskOp::Reopen | TaskOp::Dismiss { .. }
        ) {
            return failed(id, "name the item, by its key or #id");
        }
        if speaker.is_none() {
            return failed(id, "no task named, and not inside a qoop terminal");
        }
    }
    let wrote = store
        .run(move |s| {
            use qoop_store::TaskError;
            let target = match task {
                Some(task) => task,
                None => {
                    let open = match speaker {
                        Some(terminal) => s.open_task_of(i64::from(terminal))?,
                        None => None,
                    };
                    let open = open.ok_or(TaskError::NotOnBoard)?;
                    if matches!(action, TaskOp::Unlink) {
                        open.id
                    } else {
                        s.linked_item(open.id)?.ok_or(TaskError::NotOnBoard)?
                    }
                }
            };
            let mut key = None;
            let changed = match action {
                TaskOp::Describe { text } => s.describe_item(target, &text, now)?,
                TaskOp::Steps { steps } | TaskOp::PhasedSteps { steps } => {
                    s.set_steps(target, &steps, now)?
                }
                TaskOp::Check { n, done } => s.check_step(target, n, done, now)?,
                TaskOp::Link { item } => s.link_task(target, item, now)?,
                TaskOp::Unlink => s.unlink(target, now)?,
                // Its key as stored, for the line qass is told.
                TaskOp::Start => {
                    key = Some(s.start_item(target, now)?);
                    true
                }
                TaskOp::Reopen => s.reopen_item(target, now)?,
                TaskOp::Dismiss { undo } => s.dismiss_item(target, undo, now)?,
                _ => unreachable!("only board writes come here"),
            };
            Ok((target, changed, key))
        })
        .await;
    match wrote {
        Ok((task, changed, key)) => {
            if changed {
                agents.tasks_changed();
            }
            ControlResponse::TaskWritten { id, task, key }
        }
        Err(e) => store_failed(id, e),
    }
}

/// The largest plan file read: a plan, not a log.
const PLAN_BYTES: u64 = 200 * 1024;

/// A plan file, read off the async threads: a regular file up to [`PLAN_BYTES`].
async fn read_file(path: String) -> std::io::Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&path)?;
        if !meta.is_file() {
            return Err(std::io::Error::other("not a file"));
        }
        if meta.len() > PLAN_BYTES {
            return Err(std::io::Error::other(format!(
                "over {} KB: a plan, not a log",
                PLAN_BYTES / 1024
            )));
        }
        std::fs::read(&path)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

/// A plan write: link a plan file, approve it, mark it for the owner, or name the next item.
/// Approving, marking and taking are qass's; a plan file comes from an agent on the item, qass,
/// or a client that speaks for no terminal (the owner's own socket).
#[allow(clippy::too_many_arguments)]
async fn plan_write(
    id: String,
    speaker: Option<u32>,
    qass: bool,
    task: Option<i64>,
    action: TaskOp,
    agents: &AgentRegistry,
    store: &StoreHandle,
    now: i64,
) -> ControlResponse {
    use qoop_store::plan::{ApprovedBy, plan_hash};
    const QASS_ONLY: &str = "only qass does this: it reads the plan, or carries the owner's word";
    match action {
        TaskOp::Next { take } => {
            if take && !qass {
                return failed(
                    id,
                    "only qass takes work from Todo; qoop task next reads it",
                );
            }
            match store.run(move |s| s.next_item(now, take)).await {
                Ok(next) => {
                    if take && next.item.is_some() {
                        agents.tasks_changed();
                    }
                    let (item, key, title) = match next.item {
                        Some((i, k, t)) => (Some(i), Some(k), Some(t)),
                        None => (None, None, None),
                    };
                    ControlResponse::Next {
                        id,
                        item,
                        key,
                        title,
                        at_work: next.at_work,
                        skipped: next.skipped,
                    }
                }
                Err(e) => store_failed(id, e),
            }
        }
        TaskOp::ShowMe { on } => {
            if !qass {
                return failed(id, QASS_ONLY);
            }
            let Some(item) = task else {
                return failed(id, "name the item, by its key or #id");
            };
            match store.run(move |s| s.set_show_me(item, on, now)).await {
                Ok(changed) => {
                    if changed {
                        agents.tasks_changed();
                    }
                    written(id, item)
                }
                Err(e) => store_failed(id, e),
            }
        }
        TaskOp::Approve { hash, owner } => {
            if !qass {
                return failed(id, QASS_ONLY);
            }
            let Some(item) = task else {
                return failed(id, "name the item, by its key or #id");
            };
            let path = match store.run(move |s| s.plan_info(item, now)).await {
                Ok(info) => info.file.map(|f| f.path),
                Err(e) => return store_failed(id, e),
            };
            let Some(path) = path else {
                return failed(
                    id,
                    format!("#{item} has no plan file: qoop task planfile <path> links one"),
                );
            };
            // What is approved is what the file says now: never text nobody read.
            let read = match read_file(path.clone()).await {
                Ok(bytes) => plan_hash(&bytes),
                Err(e) => return failed(id, format!("{path}: {e}")),
            };
            // The whole hash, or the 8 or more characters of it a card carries.
            let hash = hash.trim().to_string();
            if hash.len() < 8 || !read.starts_with(&hash) {
                return failed(
                    id,
                    format!(
                        "{path} changed since it was read ({read} now, {hash} read): read it again"
                    ),
                );
            }
            let by = if owner {
                ApprovedBy::Owner
            } else {
                ApprovedBy::Qass
            };
            match store
                .run(move |s| s.approve_plan(item, by, &hash, now))
                .await
            {
                Ok(_) => {
                    agents.tasks_changed();
                    written(id, item)
                }
                Err(e) => store_failed(id, e),
            }
        }
        TaskOp::PlanFile { path } => {
            // An agent on the item or qass, each by its terminal's token: a client that speaks for
            // no terminal links nothing, and has the daemon read no file.
            if speaker.is_none() {
                return failed(
                    id,
                    "link a plan from the terminal of the agent on the item, or qass",
                );
            }
            if !std::path::Path::new(&path).is_absolute() {
                return failed(id, "the plan file is an absolute path");
            }
            let bytes = match read_file(path.clone()).await {
                Ok(b) => b,
                Err(e) => return failed(id, format!("{path}: {e}")),
            };
            let wrote = store
                .run(move |s| {
                    let open = match speaker {
                        Some(t) => s.open_task_of(i64::from(t))?,
                        None => None,
                    };
                    let linked = match &open {
                        Some(o) => s.linked_item(o.id)?,
                        None => None,
                    };
                    let Some(item) = task.or(linked) else {
                        return Ok(Err(
                            "this task is not on the board: qoop task link <KEY> puts it there"
                                .to_string(),
                        ));
                    };
                    // An agent links a plan to the item it works on, nothing else.
                    if speaker.is_some() && !qass && linked != Some(item) {
                        return Ok(Err(format!(
                            "#{item} is not the item this terminal's task is on"
                        )));
                    }
                    let terminal = if qass {
                        0
                    } else {
                        speaker.map_or(0, i64::from)
                    };
                    s.set_planfile(item, &path, &bytes, terminal, now)
                        .map(|hash| Ok((item, hash)))
                })
                .await;
            match wrote {
                Ok(Ok((item, hash))) => {
                    agents.tasks_changed();
                    ControlResponse::TaskWritten {
                        id,
                        task: item,
                        key: Some(hash),
                    }
                }
                Ok(Err(reason)) => failed(id, reason),
                Err(e) => store_failed(id, e),
            }
        }
        _ => unreachable!("only plan writes come here"),
    }
}

fn info(t: TaskRow, tab: Tab, waiting_at: Option<i64>) -> TaskInfo {
    let asleep_since = tab.asleep_since;
    let (state, status_at) = task_status(&t, waiting_at, tab);
    TaskInfo {
        id: t.id,
        workspace: t.workspace,
        terminal: t.terminal,
        goal: t.goal,
        stage: t.stage,
        opened_at: t.opened_at,
        updated_at: t.updated_at,
        claimed_at: t.claimed_at,
        closed_at: t.closed_at,
        outcome: t.outcome,
        folder: t.folder,
        status: state.as_str().to_string(),
        status_at,
        asleep_since,
    }
}

fn event_info(e: TaskEventRow) -> TaskEventInfo {
    TaskEventInfo {
        id: e.id,
        at: e.at,
        kind: e.kind,
        body: e.body,
    }
}

/// Where a status stands in the order a board lists loose work: what needs the owner first.
fn need_rank(status: &str) -> u8 {
    ["waits", "checks", "error", "asleep", "idle", "working"]
        .iter()
        .position(|s| *s == status)
        .map_or(6, |p| p as u8)
}

/// Keep items, in order, while the answer stays under the cap. Answers whether any was cut.
fn fit<T: serde::Serialize>(items: Vec<T>, budget: &mut usize) -> (Vec<T>, bool) {
    let mut kept = Vec::new();
    let total = items.len();
    for item in items {
        // The comma between items, and the item as it will be written.
        let size = serde_json::to_vec(&item)
            .map(|v| v.len() + 1)
            .unwrap_or(usize::MAX);
        if size > *budget {
            break;
        }
        *budget -= size;
        kept.push(item);
    }
    let cut = kept.len() < total;
    (kept, cut)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read(
    id: String,
    board: bool,
    task: Option<i64>,
    terminal: Option<u32>,
    token: Option<String>,
    before: Option<i64>,
    space: Option<String>,
    names: std::collections::BTreeMap<i64, String>,
    checked: std::collections::BTreeSet<i64>,
    agents: &AgentRegistry,
    store: &StoreHandle,
) -> ControlResponse {
    let speaker = match speaker(agents, terminal, token.as_deref()) {
        Ok(s) => s,
        Err(reason) => return failed(id, reason),
    };
    let now = now();
    let found = store
        .run(move |s| {
            let mut loose = None;
            let items = if board { Some(s.items(now)?) } else { None };
            let mut card = match task {
                Some(task) => s.item_card(task, now)?,
                None => None,
            };
            let one = match (task, speaker) {
                (Some(task), _) => Some(s.task(task)?),
                (None, Some(terminal)) => Some(s.open_task_of(i64::from(terminal))?),
                (None, None) => None,
            };
            // A terminal's own read also carries the item its open task works on: what its
            // per-request line names. Without the activity, which that line never shows and
            // which is the bulk of a card.
            if let (None, Some(Some(open))) = (task, &one)
                && let Some(item) = s.linked_item(open.id)?
            {
                card = s.item_card(item, now)?.map(|mut c| {
                    c.activity.clear();
                    c
                });
            }
            let (tasks, events, one) = match one {
                // One task (or none open), with its events; an item's are in its card's
                // activity, with its agents'. A terminal's own task keeps its events beside
                // its item's card.
                Some(row) => {
                    let events = match &row {
                        // One more than is kept, so a cut is known even below the byte cap.
                        Some(t) if task.is_none() || card.is_none() => {
                            s.task_events(t.id, before, EVENTS_READ + 1)?
                        }
                        _ => Vec::new(),
                    };
                    (row.into_iter().collect::<Vec<_>>(), events, true)
                }
                // A board's tasks are the ones it has no card for, and how many there are.
                None if board => {
                    let all = s.loose_tasks(QASS_SPACE)?;
                    loose = Some(all.len());
                    (all, Vec::new(), false)
                }
                // Every open task.
                None => (s.open_tasks()?, Vec::new(), false),
            };
            // What a status reads beyond the record: when each tab went to sleep, and when
            // each waiting task began to wait.
            let asleep: BTreeMap<i64, i64> = s
                .asleep()?
                .into_iter()
                .map(|(terminal, _, since)| (terminal, since))
                .collect();
            let waiting = s.waiting_since()?;
            Ok((items, card, tasks, events, one, loose, asleep, waiting))
        })
        .await;
    let (items, card, tasks, events, one, loose, asleep, waiting) = match found {
        Ok(found) => found,
        Err(e) => return store_failed(id, e),
    };
    let name = |t: i64| names.get(&t).cloned();
    // A tab's live row is the registry's, a sleep the store's: the same two for every agent.
    // `checked` is the daemon's: the terminals qass would check a claim in now.
    let tab = |terminal: i64| {
        let live = u32::try_from(terminal).ok().and_then(|t| agents.live(t));
        tab_of(live, &asleep, terminal, checked.contains(&terminal))
    };
    let items: Option<Vec<_>> =
        items.map(|v| v.into_iter().map(|r| item_info(r, name, tab)).collect());
    let card = card.map(|c| ItemCardInfo {
        item: item_info(c.item, name, tab),
        description: c.description,
        steps: c
            .steps
            .into_iter()
            .map(|s| StepInfo {
                n: s.n,
                text: s.text,
                done_at: s.done_at,
            })
            .collect(),
        activity: c
            .activity
            .into_iter()
            .map(|e| ItemEventInfo {
                task: e.task,
                at: e.at,
                kind: e.kind,
                body: e.body,
            })
            .collect(),
        plan_file: c.plan.map(|p| crate::control::PlanFileInfo {
            path: p.path,
            hash: p.hash,
            terminal: p.terminal,
            summary: p.summary,
            at: p.at,
        }),
    });
    if one && task.is_some() && tasks.is_empty() {
        return failed(id, format!("no task #{}", task.unwrap_or_default()));
    }
    let read_cut = events.len() > EVENTS_READ;
    let events: Vec<TaskEventRow> = events.into_iter().take(EVENTS_READ).collect();
    // Room for the envelope first: the fixed keys, and the id and space the caller chose,
    // which are as long as they are. Then rows, then events.
    // Measured, not guessed: an id is the caller's and JSON may write a character of it as
    // six bytes.
    // A card's description and steps are in the envelope: they are bounded, and a card
    // without them is not one. Its activity is fitted last.
    let mut card = card;
    let activity = card
        .as_mut()
        .map(|c| std::mem::take(&mut c.activity))
        .unwrap_or_default();
    let envelope = serde_json::to_vec(&ControlResponse::Tasks {
        id: id.clone(),
        tasks: Vec::new(),
        events: Vec::new(),
        more: true,
        space: space.clone(),
        items: items.as_ref().map(|_| Vec::new()),
        item: card.clone().map(Box::new),
        loose: loose.map(|_| 0),
    })
    .map_or(ANSWER_BYTES, |v| v.len() + 64);
    let mut budget = ANSWER_BYTES.saturating_sub(envelope);
    // Items first: a board that outgrows the frame loses loose tasks before it loses a card.
    let (items, items_cut) = match items {
        Some(items) => {
            let (kept, cut) = fit(items, &mut budget);
            (Some(kept), cut)
        }
        None => (None, false),
    };
    let mut tasks: Vec<TaskInfo> = tasks
        .into_iter()
        .map(|t| {
            let tab = t.terminal.map(tab).unwrap_or_default();
            let waiting_at = waiting.get(&t.id).copied();
            info(t, tab, waiting_at)
        })
        .collect();
    if loose.is_some() {
        // What the folded row lists: the tasks that need him first, so a cut loses the
        // quiet ones; a stable sort keeps each rank newest first. A line carries a short
        // goal, as the agents listing does.
        tasks.sort_by_key(|t| need_rank(&t.status));
        for t in &mut tasks {
            t.goal = t.goal.chars().take(TASK_REF_GOAL).collect();
        }
    }
    let (tasks, rows_cut) = fit(tasks, &mut budget);
    let (events, events_cut) = fit(events.into_iter().map(event_info).collect(), &mut budget);
    let (activity, activity_cut) = fit(activity, &mut budget);
    let item = card.map(|mut c| {
        c.activity = activity;
        Box::new(c)
    });
    ControlResponse::Tasks {
        id,
        tasks,
        events,
        more: rows_cut || items_cut || events_cut || activity_cut || read_cut,
        space,
        items,
        item,
        loose,
    }
}

/// The agents listing's view of the open tasks, by terminal.
pub(crate) fn refs(open: Vec<TaskRow>) -> std::collections::BTreeMap<u32, TaskRef> {
    open.into_iter()
        .filter_map(|t| {
            let terminal = u32::try_from(t.terminal?).ok()?;
            Some((
                terminal,
                TaskRef {
                    id: t.id,
                    goal: t.goal.chars().take(TASK_REF_GOAL).collect(),
                    stage: t.stage,
                    claimed: t.claimed_at.is_some(),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link from a terminal begins its task first; where a begin opens nothing it is refused
    /// with the reason and leaves no task behind: qass's own space, and a space whose name was
    /// not read in time. With the name read, it links.
    #[tokio::test]
    async fn a_link_where_a_begin_opens_nothing_is_refused_with_why() {
        let agents = AgentRegistry::new().expect("registry");
        let token = crate::agents::mint_token(1);
        agents.register(1, TerminalId::new("1"), token.clone());
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).unwrap();
        let (item, _) = s.add_item(ws, "the item", "", None, 1).unwrap();
        let store = StoreHandle::new(s);
        let link = |space: Option<&str>| {
            write(
                "w".into(),
                Some(1),
                Some(token.as_str().to_string()),
                None,
                TaskOp::Link { item },
                space.map(String::from),
                Some("worker".to_string()),
                None,
                &agents,
                &store,
            )
        };
        for (space, why) in [
            (None, "could not read this terminal's space"),
            (Some(QASS_SPACE), "qass's own space keeps no task"),
        ] {
            let answer = link(space).await;
            assert!(
                matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains(why)),
                "{answer:?}"
            );
        }
        let open = store.run(|s| s.open_tasks()).await.unwrap();
        assert!(open.is_empty(), "nothing begun: {open:?}");
        let answer = link(Some("work")).await;
        let ControlResponse::TaskWritten { task, .. } = answer else {
            panic!("not linked: {answer:?}")
        };
        let linked = store.run(move |s| s.linked_item(task)).await.unwrap();
        assert_eq!(linked, Some(item));
    }

    /// A task a link opens is named after the item, not the agent's last prompt, and says it
    /// was opened by the link. Once closed, its agent keeps the name it was linked under on
    /// the item, though its terminal is gone, and its stage reads finished.
    #[tokio::test]
    async fn a_linked_task_is_the_items_and_its_agent_keeps_its_name_once_closed() {
        let agents = AgentRegistry::new().expect("registry");
        let token = crate::agents::mint_token(1);
        agents.register(1, TerminalId::new("1"), token.clone());
        agents.push_focus_event(
            1,
            crate::control::FocusEvent {
                kind: "user".into(),
                label: "check what the tests say".into(),
                at_unix: 1,
            },
        );
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).unwrap();
        let (item, key) = s
            .add_item(ws, "login keeps the session", "", None, 1)
            .unwrap();
        let store = StoreHandle::new(s);
        let answer = write(
            "w".into(),
            Some(1),
            Some(token.as_str().to_string()),
            None,
            TaskOp::Link { item },
            Some("work".into()),
            Some("worker".into()),
            None,
            &agents,
            &store,
        )
        .await;
        let ControlResponse::TaskWritten { task, .. } = answer else {
            panic!("not linked: {answer:?}")
        };
        let (row, events) = store
            .run(move |s| Ok((s.task(task)?, s.task_events(task, None, 10)?)))
            .await
            .unwrap();
        assert_eq!(row.unwrap().goal, "login keeps the session");
        let opened = events.iter().find(|e| e.kind == "opened").unwrap();
        assert_eq!(
            opened.body,
            format!("opened by a link to {key}: login keeps the session")
        );

        let live = |t: i64| (t == 1).then(|| "its tab".to_string());
        let read = || store.run(move |s| s.items(10));
        let open =
            crate::control::item_info(read().await.unwrap().remove(0), live, |_| Tab::default());
        assert_eq!(open.agents[0].name, "its tab", "open: the live name");
        store.run(|s| s.close_terminal(1, 5)).await.unwrap();
        // The terminal is gone, and its number may name another tab now.
        let closed =
            crate::control::item_info(read().await.unwrap().remove(0), live, |_| Tab::default());
        let agent = &closed.agents[0];
        assert!(agent.closed);
        assert_eq!(
            (agent.name.as_str(), agent.stage.as_str()),
            ("worker", crate::control::FINISHED)
        );
    }

    /// A board's `tasks` are the open ones no open item has, outside qass's space, those that
    /// need him first and then newest first, each with its status and a short goal, and
    /// `loose` counts them all. Items are fitted first: four hundred long tasks cut the
    /// tasks, never the item, and never the one that waits on him, though it is the oldest.
    #[tokio::test]
    async fn a_board_read_carries_the_loose_tasks_newest_first_and_never_loses_an_item() {
        let agents = AgentRegistry::new().expect("registry");
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        let qass = s.create_workspace(QASS_SPACE, 1).unwrap();
        let (item, _) = s.add_item(ws, "the item", "", None, 1).unwrap();
        let t = s.create_terminal(1, ws, "/tmp", "/bin/sh", 2).unwrap();
        let linked = s.open_task(t, "on the item", false, 3).unwrap();
        s.link_task(linked, item, 4).unwrap();
        let q = s.create_terminal(2, qass, "/tmp", "/bin/sh", 2).unwrap();
        s.open_task(q, "in qass's space", false, 5).unwrap();
        let goal = "x".repeat(qoop_store::task::GOAL_MAX);
        let mut made = Vec::new();
        for n in 10..410 {
            let t = s.create_terminal(n, ws, "/tmp", "/bin/sh", 6).unwrap();
            made.push(s.open_task(t, &goal, false, 100 + n).unwrap());
        }
        let waiting = made[0];
        s.set_task_stage(waiting, "waiting-human", 900).unwrap();
        let store = StoreHandle::new(s);
        let answer = read(
            "r".into(),
            true,
            None,
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeSet::new(),
            &agents,
            &store,
        )
        .await;
        let ControlResponse::Tasks {
            tasks,
            items,
            loose,
            more,
            ..
        } = answer
        else {
            panic!("not a board: {answer:?}")
        };
        assert_eq!(items.expect("the board").len(), 1, "the item survives");
        assert_eq!(
            loose,
            Some(400),
            "the true count, linked and qass's left out"
        );
        assert!(
            more && !tasks.is_empty() && tasks.len() < 400,
            "{}",
            tasks.len()
        );
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        let mut rest: Vec<i64> = made[1..].iter().rev().copied().collect();
        rest.truncate(ids.len() - 1);
        assert_eq!(ids[0], waiting, "the wait leads, though it is the oldest");
        assert_eq!(tasks[0].status, "waits");
        assert_eq!(ids[1..], rest, "then newest first");
        assert!(tasks.iter().all(|t| !t.status.is_empty()));
        assert!(
            tasks
                .iter()
                .all(|t| t.goal.chars().count() <= TASK_REF_GOAL)
        );
        // Read as a task of its own it keeps its whole goal.
        let answer = read(
            "r".into(),
            false,
            Some(waiting),
            None,
            None,
            None,
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeSet::new(),
            &agents,
            &store,
        )
        .await;
        let ControlResponse::Tasks { tasks, loose, .. } = answer else {
            panic!("not a task: {answer:?}")
        };
        assert_eq!(loose, None);
        assert_eq!(tasks[0].goal.chars().count(), qoop_store::task::GOAL_MAX);
        assert_eq!(
            (tasks[0].status.as_str(), tasks[0].status_at),
            ("waits", 900)
        );
    }

    /// Rows and events alike stop where the next one would pass the budget, in order, and
    /// say so.
    #[test]
    fn fit_keeps_whole_items_in_order_until_the_budget_is_spent() {
        let rows: Vec<String> = (0..10).map(|n| format!("{n}{}", "x".repeat(98))).collect();
        let one = serde_json::to_vec(&rows[0]).unwrap().len() + 1;
        let mut budget = one * 4 + one / 2;
        let (kept, cut) = fit(rows.clone(), &mut budget);
        assert_eq!(kept, rows[..4].to_vec());
        assert!(cut);
        assert!(budget < one, "what is left is less than one more item");
        let mut plenty = usize::MAX / 2;
        let (all, cut) = fit(rows.clone(), &mut plenty);
        assert_eq!(all.len(), 10);
        assert!(!cut);
    }

    /// A done claim's record keeps the program, the session id and where it was, and the
    /// checkout's `HEAD` and whether it was clean at the claim; what is not known is left out,
    /// never guessed. Untracked files count though the checkout's config hides them, a
    /// submodule moved is a change, and a checkout with submodules is never called clean.
    #[tokio::test]
    async fn a_record_keeps_the_checkouts_head_and_whether_it_was_clean() {
        let dir = tempfile::tempdir().unwrap();
        let git = |at: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(at)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "protocol.file.allow=always",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let repo = dir.path().join("repo");
        let sub = dir.path().join("sub");
        for at in [&repo, &sub] {
            std::fs::create_dir(at).unwrap();
            git(at, &["init", "-q"]);
            std::fs::write(at.join("a"), "a").unwrap();
            git(at, &["add", "a"]);
            git(at, &["commit", "-qm", "a"]);
        }
        git(&repo, &["config", "status.showUntrackedFiles", "no"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let cwd = Some(repo.display().to_string());
        let read = |r: String| serde_json::from_str::<serde_json::Value>(&r).unwrap();

        // A Claude transcript under a `projects` directory of the dirs given.
        let home = dir.path().join("home");
        let project = home.join(".claude/projects/-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("s1.jsonl"), "{}\n").unwrap();
        let file = project.join("s1.jsonl").canonicalize().unwrap();
        let dirs = qoop_parsed::Dirs::under(&home, None, None);
        let clean = read(
            record(
                Some(dirs),
                Some(4),
                true,
                Some("claude".into()),
                Some("s1".into()),
                cwd.clone(),
            )
            .await,
        );
        assert_eq!(
            clean,
            serde_json::json!({"terminal": 4, "skipped": true, "program": "claude",
                "session": "s1", "source": {"file": file}, "head": head, "clean": true})
        );
        std::fs::write(repo.join("b"), "untracked").unwrap();
        let dirty = read(record(None, None, false, None, None, cwd.clone()).await);
        assert_eq!(dirty, serde_json::json!({"head": head, "clean": false}));
        std::fs::remove_file(repo.join("b")).unwrap();

        let sub_url = sub.display().to_string();
        git(&repo, &["submodule", "add", "-q", &sub_url, "sub"]);
        git(&repo, &["commit", "-qm", "sub"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let with_sub = read(record(None, None, false, None, None, cwd.clone()).await);
        assert_eq!(
            with_sub,
            serde_json::json!({"head": head}),
            "not called clean"
        );
        let inner = repo.join("sub");
        std::fs::write(inner.join("c"), "c").unwrap();
        git(&inner, &["add", "c"]);
        git(&inner, &["commit", "-qm", "c"]);
        let moved = read(record(None, None, false, None, None, cwd).await);
        assert_eq!(moved, serde_json::json!({"head": head, "clean": false}));

        let nowhere = read(
            record(
                None,
                None,
                false,
                None,
                None,
                Some(dir.path().display().to_string()),
            )
            .await,
        );
        assert_eq!(nowhere, serde_json::json!({}), "no checkout, nothing said");
    }

    /// An item added from a coding agent's tab in a work space waits for the owner's start
    /// (`added_by`); one qass adds for him, or one he adds from a shell tab, does not.
    #[tokio::test]
    async fn only_an_agent_marks_the_item_it_adds_for_later() {
        let agents = AgentRegistry::new().expect("registry");
        let mut tokens = Vec::new();
        for t in [1, 2, 3] {
            let token = crate::agents::mint_token(t);
            agents.register(t, TerminalId::new(t.to_string()), token.clone());
            tokens.push(token.as_str().to_string());
        }
        agents.test_set_foreground(1, Some("claude"), false);
        agents.test_set_foreground(2, Some("claude"), false);
        agents.test_set_foreground(3, Some("zsh"), false);
        let mut s = qoop_store::Store::in_memory().expect("store");
        let work = s.create_workspace("work", 1).unwrap();
        let qass = s.create_workspace(QASS_SPACE, 1).unwrap();
        s.create_terminal(1, work, "/tmp", "/bin/sh", 1).unwrap();
        s.create_terminal(2, qass, "/tmp", "/bin/sh", 1).unwrap();
        s.create_terminal(3, work, "/tmp", "/bin/sh", 1).unwrap();
        let store = StoreHandle::new(s);
        for (terminal, by) in [(1u32, Some("T1")), (2, None), (3, None)] {
            let answer = write(
                "w".into(),
                Some(terminal),
                Some(tokens[terminal as usize - 1].clone()),
                None,
                TaskOp::Add {
                    workspace: Some(work),
                    goal: format!("later {terminal}"),
                    description: None,
                },
                None,
                None,
                None,
                &agents,
                &store,
            )
            .await;
            let ControlResponse::TaskWritten { task, .. } = answer else {
                panic!("{answer:?}")
            };
            let marks = store
                .run(move |s| s.task_events_of(task, "added_by", 1))
                .await
                .unwrap();
            assert_eq!(
                marks.first().map(|e| e.body.as_str()),
                by,
                "terminal {terminal}"
            );
        }
    }

    /// A named close records who asked and nothing else changes: qass's terminal reads `qass`,
    /// another with its own token `T<n>/<generation>`, and one with no identity or a wrong
    /// token `unknown`; each still closes the task, whoever's it is.
    #[tokio::test]
    async fn a_close_records_who_asked_and_admits_as_before() {
        let agents = AgentRegistry::new().expect("registry");
        let mut tokens = Vec::new();
        for t in [1, 2] {
            let token = crate::agents::mint_token(t);
            agents.register(t, TerminalId::new(t.to_string()), token.clone());
            tokens.push(token.as_str().to_string());
        }
        let generation = agents.agent(2).unwrap().belief.generation;
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).unwrap();
        let store = StoreHandle::new(s);
        let actor = |terminal: u32, token: &str| {
            Some(Actor {
                terminal,
                token: token.to_string(),
            })
        };
        let cases = [
            (actor(1, &tokens[0]), Some(QASS_SPACE), "qass".to_string()),
            (
                actor(2, &tokens[1]),
                Some("work"),
                format!("T2/{generation}"),
            ),
            (actor(2, &tokens[0]), Some("work"), "unknown".to_string()),
            (None, None, "unknown".to_string()),
        ];
        for (n, (actor, space, by)) in cases.into_iter().enumerate() {
            let task = store
                .run(move |s| s.open_task(1, &format!("part {n}"), true, 2))
                .await
                .unwrap();
            let answer = write(
                "w".into(),
                None,
                None,
                Some(task),
                TaskOp::Close {
                    outcome: crate::control::TaskOutcome::Verified,
                    reason: "checked".into(),
                    actor,
                },
                space.map(String::from),
                None,
                None,
                &agents,
                &store,
            )
            .await;
            assert!(
                matches!(answer, ControlResponse::TaskWritten { .. }),
                "{answer:?}"
            );
            let closed = store
                .run(move |s| s.task_events_of(task, "closed", 1))
                .await
                .unwrap();
            assert_eq!(closed[0].body, format!("verified by {by}: checked"));
        }
    }

    /// `qoop worktree new` from qass's space marks the terminal it opened; from any other
    /// space it marks nothing, and says nothing wrong either.
    #[tokio::test]
    async fn only_qass_marks_the_terminal_it_starts() {
        let agents = AgentRegistry::new().expect("registry");
        let token = crate::agents::mint_token(1);
        agents.register(1, TerminalId::new("1"), token.clone());
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).unwrap();
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).unwrap();
        let store = StoreHandle::new(s);
        let mark = |space: &str| {
            write(
                "w".into(),
                Some(1),
                Some(token.as_str().to_string()),
                None,
                TaskOp::Spawned { terminal: 7 },
                Some(space.to_string()),
                None,
                None,
                &agents,
                &store,
            )
        };
        let opened = |goal: &'static str| {
            store.run(move |s| {
                let task = s.open_task(7, goal, true, 2)?;
                s.task_events_of(task, "spawned_by", 1)
            })
        };
        assert!(matches!(
            mark("work").await,
            ControlResponse::TaskWritten { .. }
        ));
        assert!(opened("a").await.unwrap().is_empty(), "not qass's");
        assert!(matches!(
            mark(QASS_SPACE).await,
            ControlResponse::TaskWritten { .. }
        ));
        assert_eq!(opened("b").await.unwrap()[0].body, "qass T7");
    }
}
