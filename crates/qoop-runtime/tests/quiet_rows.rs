//! `quiet_rows`: a phone hears that a row changed only when what it shows of the row moved.
//! Who watches a tab (`clients`), a tool's label and the like are told to every other client
//! and not to it; a new state is told to it too.

use std::time::Duration;

use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

/// What the index says `terminal` is doing.
async fn state(client: &mut TestClient, terminal: u32) -> String {
    match client
        .control(
            &ControlRequest::AgentRows {
                id: "state".into(),
                terminals: vec![terminal],
            },
            PATIENCE,
        )
        .await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .next()
            .map(|row| row.state)
            .unwrap_or_default(),
        other => panic!("no row: {other:?}"),
    }
}

#[tokio::test]
async fn a_quiet_phone_hears_a_row_only_when_what_it_shows_moved() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut hook = d.attach("hook").await;
    let mut loud = d.attach("watcher").await;
    // The shell's prompt read first, so nothing moves on its own below.
    let deadline = std::time::Instant::now() + PATIENCE;
    while state(&mut loud, terminal).await != "idle" {
        assert!(std::time::Instant::now() < deadline, "never idle");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut quiet = d
        .attach_phone_with(serde_json::json!({ qoop_runtime::client::QUIET_ROWS: true }))
        .await;
    // What the phone holds, read the way it reads it.
    let listed = quiet
        .control(&ControlRequest::ListAgents { id: "rows".into() }, PATIENCE)
        .await;
    assert!(
        matches!(listed, Some(ControlResponse::Agents { .. })),
        "{listed:?}"
    );
    quiet
        .wait_for_event(|_| false, Duration::from_millis(1500))
        .await;
    quiet.forget_events();
    loud.forget_events();

    // Another phone opens the tab and leaves it: the row's watchers move, nothing it shows.
    let ours = |e: &EventMessage| matches!(e, EventMessage::AgentChanged { terminal: t } if *t == terminal);
    let mut other = d.attach_phone().await;
    for (id, request) in [
        (
            "on",
            ControlRequest::Subscribe {
                id: "on".into(),
                terminals: vec![terminal],
            },
        ),
        (
            "off",
            ControlRequest::Unsubscribe {
                id: "off".into(),
                terminals: vec![terminal],
            },
        ),
    ] {
        assert!(other.control(&request, PATIENCE).await.is_some(), "{id}");
    }
    assert!(
        loud.wait_for_event(ours, PATIENCE).await.is_some(),
        "every other client hears the watchers move"
    );
    assert!(
        quiet
            .wait_for_event(ours, Duration::from_secs(3))
            .await
            .is_none(),
        "a quiet phone does not"
    );

    // A question: the state moves, and the quiet phone is told.
    let answer = hook
        .control(
            &ControlRequest::ReportAgentState {
                from: None,
                id: "ask".into(),
                terminal,
                token,
                state: "blocked".into(),
                visible_blocker: true,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: None,
                label: None,
                said: None,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(answer, Some(ControlResponse::AgentStateReported { .. })),
        "{answer:?}"
    );
    assert!(
        quiet
            .wait_for_event(ours, Duration::from_secs(5))
            .await
            .is_some(),
        "a new state is news to it"
    );
}

#[tokio::test]
async fn a_quiet_phone_hears_a_task_open_and_close() {
    use qoop_runtime::control::{TaskOp, TaskOutcome};
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut hook = d.attach("hook").await;
    let deadline = std::time::Instant::now() + PATIENCE;
    while state(&mut hook, terminal).await != "idle" {
        assert!(std::time::Instant::now() < deadline, "never idle");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut quiet = d
        .attach_phone_with(serde_json::json!({ qoop_runtime::client::QUIET_ROWS: true }))
        .await;
    let listed = quiet
        .control(&ControlRequest::ListAgents { id: "rows".into() }, PATIENCE)
        .await;
    assert!(
        matches!(listed, Some(ControlResponse::Agents { .. })),
        "{listed:?}"
    );
    quiet
        .wait_for_event(|_| false, Duration::from_millis(1500))
        .await;
    quiet.forget_events();
    let ours = |e: &EventMessage| matches!(e, EventMessage::AgentChanged { terminal: t } if *t == terminal);

    let opened = hook
        .control(
            &ControlRequest::TaskWrite {
                id: "begin".into(),
                terminal: Some(terminal),
                token: Some(token),
                task: None,
                action: TaskOp::Begin,
            },
            PATIENCE,
        )
        .await;
    let Some(ControlResponse::TaskWritten { task, .. }) = opened else {
        panic!("no task: {opened:?}");
    };
    assert!(
        quiet
            .wait_for_event(ours, Duration::from_secs(5))
            .await
            .is_some(),
        "a task opened is news"
    );
    // Read as the phone reads it after the news, which is what it holds now.
    let read = quiet
        .control(
            &ControlRequest::AgentRows {
                id: "row".into(),
                terminals: vec![terminal],
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(&read, Some(ControlResponse::Agents { agents, .. }) if agents[0].task.is_some()),
        "{read:?}"
    );
    quiet
        .wait_for_event(|_| false, Duration::from_millis(1500))
        .await;
    quiet.forget_events();

    let closed = hook
        .control(
            &ControlRequest::TaskWrite {
                id: "close".into(),
                terminal: None,
                token: None,
                task: Some(task),
                action: TaskOp::Close {
                    outcome: TaskOutcome::Verified,
                    reason: String::new(),
                    actor: None,
                },
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(closed, Some(ControlResponse::TaskWritten { .. })),
        "{closed:?}"
    );
    assert!(
        quiet
            .wait_for_event(ours, Duration::from_secs(5))
            .await
            .is_some(),
        "a task closed is news"
    );
}
