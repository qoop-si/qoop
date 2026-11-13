//! A tab waiting on its person is said once. A blocker that holds is not news again, so a
//! client that lists every row on each `AgentChanged` (a phone, over a metered link) pays for
//! one listing, not one every 800 ms for as long as the question waits.

use std::time::Duration;

use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage};

#[tokio::test]
async fn a_held_blocker_is_announced_once() {
    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut hook = d.attach("hook").await;
    let mut index = d.attach("qoop-watch").await;
    // The shell's own prompt read first: a look at a bare prompt says idle, and one landing
    // after the report would take the blocker back.
    let deadline = std::time::Instant::now() + qoop_testkit::PATIENCE;
    while state(&mut index, terminal).await != "idle" {
        assert!(
            std::time::Instant::now() < deadline,
            "the prompt never read idle"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(answer, Some(ControlResponse::AgentStateReported { .. })),
        "{answer:?}"
    );
    let ours = |e: &EventMessage| matches!(e, EventMessage::AgentChanged { terminal: t } if *t == terminal);
    assert!(
        index
            .wait_for_event(ours, qoop_testkit::PATIENCE)
            .await
            .is_some(),
        "the blocker is said"
    );
    // A key without Enter: the screen is looked at again while the blocker holds, which is
    // where a repeat would start. What the key itself says goes by first.
    hook.input_to(terminal, b"x").await;
    index
        .wait_for_event(|_| false, Duration::from_millis(1500))
        .await;
    index.forget_events();
    assert!(
        index
            .wait_for_event(ours, Duration::from_secs(3))
            .await
            .is_none(),
        "a blocker that holds is not announced again"
    );
    assert_eq!(state(&mut index, terminal).await, "blocked");
}

/// What the index says `terminal` is doing.
async fn state(index: &mut qoop_testkit::TestClient, terminal: u32) -> String {
    match index
        .control(
            &ControlRequest::ListAgents { id: "rows".into() },
            qoop_testkit::PATIENCE,
        )
        .await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .map(|row| row.state)
            .unwrap_or_default(),
        other => panic!("no index: {other:?}"),
    }
}
