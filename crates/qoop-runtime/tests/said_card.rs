//! A turn's closing words travel from the report to the owner's card: the whole question and
//! what the agent did just before, never the cut label alone.

use qoop_runtime::control::{ControlRequest, ControlResponse};

#[tokio::test]
async fn a_turns_closing_words_make_its_card() {
    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    // A Mac with qass, and the agent's task already waiting on the owner (a wait it set).
    let mut client = d.attach("hook").await;
    d.store()
        .create_workspace("qass".into())
        .await
        .expect("qass's space");
    for (id, action) in [
        ("new", qoop_runtime::control::TaskOp::Begin),
        (
            "wait",
            qoop_runtime::control::TaskOp::Stage {
                stage: "waiting-human".into(),
            },
        ),
    ] {
        let wrote = client
            .control(
                &ControlRequest::TaskWrite {
                    id: id.into(),
                    terminal: Some(terminal),
                    token: Some(token.clone()),
                    task: None,
                    action,
                },
                qoop_testkit::PATIENCE,
            )
            .await;
        assert!(
            matches!(wrote, Some(ControlResponse::TaskWritten { .. })),
            "{id}: {wrote:?}"
        );
    }
    let question = format!(
        "Shall I merge PR #513 now and deploy right after, or {}wait for your look?",
        "really ".repeat(25)
    );
    let answer = client
        .control(
            &ControlRequest::ReportAgentState {
                from: None,
                id: "stop".into(),
                terminal,
                token,
                state: "idle".into(),
                visible_blocker: false,
                visible_idle: true,
                visible_working: false,
                clock: 0,
                session: None,
                event: Some("done".into()),
                label: Some("Shall I merge PR #513 now ... wait for your look?".into()),
                said: Some(format!("Built the table, 208 tests pass.\n\n{question}").into()),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        !matches!(answer, Some(ControlResponse::Failed { .. }) | None),
        "{answer:?}"
    );
    let mut cards = Vec::new();
    for _ in 0..50 {
        if let Some(ControlResponse::Asks { live, .. }) = client
            .control(
                &ControlRequest::AskRead {
                    id: "read".into(),
                    all: false,
                },
                qoop_testkit::PATIENCE,
            )
            .await
        {
            cards = live;
        }
        if !cards.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(cards.len(), 1, "{cards:?}");
    let body = &cards[0].body;
    assert_eq!(body.asks, question, "{body:?}");
    assert_eq!(body.doing, "Built the table, 208 tests pass.", "{body:?}");
}
