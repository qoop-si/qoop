//! The daemon's side of `qoop task`: which task a request means, the write, and an answer
//! that fits one frame.
//!
//! A terminal names its own task through its token, checked like a hook report: a token
//! that does not match refuses the call, with no guessing. A task named by id needs no
//! terminal, because the socket is the owner's own. Who closes a task is a convention (qass
//! or the person), not a check.

use qoop_domain::{CapabilityToken, TerminalId};
use qoop_store::{Outcome, TaskEventRow, TaskRow};

use crate::agents::AgentRegistry;
use crate::control::{
    ControlResponse, ItemCardInfo, ItemEventInfo, StepInfo, TASK_REF_GOAL, TaskEventInfo, TaskInfo,
    TaskOp, TaskOutcome, TaskRef, item_info,
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write(
    id: String,
    terminal: Option<u32>,
    token: Option<String>,
    task: Option<i64>,
    action: TaskOp,
    space: Option<String>,
    name: Option<String>,
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
        return match store
            .run(move |s| {
                let name = s
                    .workspaces()?
                    .into_iter()
                    .find(|w| w.id == workspace)
                    .map(|w| w.name);
                if name.as_deref() == Some(QASS_SPACE) {
                    return Ok(None);
                }
                let description = description.unwrap_or_default();
                s.add_item(workspace, &goal, &description, now).map(Some)
            })
            .await
        {
            Ok(Some((task, key))) => {
                agents.tasks_changed();
                ControlResponse::TaskWritten {
                    id,
                    task,
                    key: Some(key),
                }
            }
            Ok(None) => failed(
                id,
                "qass's own space has no board: add the item to the space the work is in",
            ),
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
        TaskOp::Describe { .. }
            | TaskOp::Steps { .. }
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
            | TaskOp::Check { .. }
            | TaskOp::Link { .. }
            | TaskOp::Unlink
            | TaskOp::Start
            | TaskOp::Reopen
            | TaskOp::Dismiss { .. }
            | TaskOp::LinkRemote { .. } => unreachable!("handled above"),
            TaskOp::Stage { stage } => s.set_task_stage(task, &stage, now),
            TaskOp::Goal { goal } => s.set_task_goal(task, &goal, now),
            TaskOp::Plan { text } => s.task_plan(task, &text, now).map(|()| false),
            TaskOp::Evidence { text } => s.task_evidence(task, &text, now).map(|()| false),
            TaskOp::Done { summary } => s.claim_task_done(task, &summary, now).map(|()| true),
            TaskOp::Close { outcome, reason } => {
                let outcome = match outcome {
                    TaskOutcome::Verified => Outcome::Verified,
                    TaskOutcome::Failed => Outcome::Failed,
                    // Items only: a task ends with its agent's work, it is not taken off a board.
                    TaskOutcome::Dropped => {
                        s.open_item_goal(task)?;
                        Outcome::Abandoned
                    }
                };
                s.close_task(task, outcome, &reason, now).map(|_| true)
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
                TaskOp::Steps { steps } => s.set_steps(target, &steps, now)?,
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

fn info(t: TaskRow) -> TaskInfo {
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
                // Every open task.
                None => (s.open_tasks()?, Vec::new(), false),
            };
            Ok((items, card, tasks, events, one))
        })
        .await;
    let (items, card, tasks, events, one) = match found {
        Ok(found) => found,
        Err(e) => return store_failed(id, e),
    };
    let name = |t: i64| names.get(&t).cloned();
    let items: Option<Vec<_>> = items.map(|v| v.into_iter().map(|r| item_info(r, name)).collect());
    let card = card.map(|c| ItemCardInfo {
        item: item_info(c.item, name),
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
    })
    .map_or(ANSWER_BYTES, |v| v.len() + 64);
    let mut budget = ANSWER_BYTES.saturating_sub(envelope);
    let (tasks, rows_cut) = fit(tasks.into_iter().map(info).collect(), &mut budget);
    let (items, items_cut) = match items {
        Some(items) => {
            let (kept, cut) = fit(items, &mut budget);
            (Some(kept), cut)
        }
        None => (None, false),
    };
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
        let (item, _) = s.add_item(ws, "the item", "", 1).unwrap();
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
        let (item, key) = s.add_item(ws, "login keeps the session", "", 1).unwrap();
        let store = StoreHandle::new(s);
        let answer = write(
            "w".into(),
            Some(1),
            Some(token.as_str().to_string()),
            None,
            TaskOp::Link { item },
            Some("work".into()),
            Some("worker".into()),
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
        let open = crate::control::item_info(read().await.unwrap().remove(0), live);
        assert_eq!(open.agents[0].name, "its tab", "open: the live name");
        store.run(|s| s.close_terminal(1, 5)).await.unwrap();
        // The terminal is gone, and its number may name another tab now.
        let closed = crate::control::item_info(read().await.unwrap().remove(0), live);
        let agent = &closed.agents[0];
        assert!(agent.closed);
        assert_eq!(
            (agent.name.as_str(), agent.stage.as_str()),
            ("worker", crate::control::FINISHED)
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
}
