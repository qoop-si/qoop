//! `PersonAt`: until when a person is at a terminal. Only a person's own connection (the
//! interface or the phone) stamps it; `qoop prompt`, `qoop send` and `qoop new` type for a
//! program and never do, so qass's own writes never hold qass back.

use std::time::Duration;

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{TestClient, TestDaemon};

const PATIENCE: Duration = Duration::from_secs(5);

async fn until(asker: &mut TestClient, terminal: u32) -> Option<i64> {
    match asker
        .control(
            &ControlRequest::PersonAt {
                id: "p".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
    {
        Some(ControlResponse::PersonAt { until, .. }) => until,
        other => panic!("no answer: {other:?}"),
    }
}

/// The daemon notes keys on its own clock; a moment to land them before asking.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn only_a_person_s_keys_hold_a_terminal() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut asker = d.attach("qoop-agents").await;
    assert_eq!(until(&mut asker, terminal).await, None, "nobody yet");

    for program in ["qoop-prompt", "qoop-send", "qoop-new", "qoop-cli"] {
        let mut writer = d.attach(program).await;
        writer.input_to(terminal, b"from a program\r").await;
        settle().await;
        assert_eq!(
            until(&mut asker, terminal).await,
            None,
            "{program} is no person"
        );
    }

    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    ui.input_to(terminal, b"\x1b[<0;3;4M").await;
    settle().await;
    assert_eq!(
        until(&mut asker, terminal).await,
        None,
        "a mouse report is no key"
    );
    ui.input_to(terminal, b"half a sent").await;
    settle().await;
    let draft = until(&mut asker, terminal).await.expect("a draft holds it");
    ui.input_to(terminal, b"ence\r").await;
    settle().await;
    let line = until(&mut asker, terminal).await.expect("a line holds it");
    assert!(
        line < draft + 1,
        "a sent line holds less long than a draft: {line} {draft}"
    );
}

#[tokio::test]
async fn the_phone_is_a_person() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut asker = d.attach("qoop-agents").await;
    let mut phone = d.attach_phone().await;
    phone.input_to(terminal, b"y").await;
    settle().await;
    assert!(
        until(&mut asker, terminal).await.is_some(),
        "the phone typed"
    );
}
