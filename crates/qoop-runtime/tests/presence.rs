//! Who has a terminal open: one kind per connection that a person sees it through (the
//! interface, the phone app, `qoop attach`), and which kind a person last typed from. A
//! program's connection never counts, and neither does the terminal a connection is handed by
//! default, except for `qoop attach`, which shows it.

use std::time::Duration;

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn row(asker: &mut TestClient, terminal: u32) -> AgentRow {
    match asker
        .control(&ControlRequest::ListAgents { id: "l".into() }, PATIENCE)
        .await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .expect("the terminal is listed"),
        other => panic!("no listing: {other:?}"),
    }
}

async fn watch(client: &mut TestClient, terminal: u32, on: bool) {
    let request = if on {
        ControlRequest::Subscribe {
            id: "s".into(),
            terminals: vec![terminal],
        }
    } else {
        ControlRequest::Unsubscribe {
            id: "u".into(),
            terminals: vec![terminal],
        }
    };
    assert!(client.control(&request, PATIENCE).await.is_some());
}

async fn clients(asker: &mut TestClient, terminal: u32) -> Vec<String> {
    row(asker, terminal).await.clients
}

#[tokio::test]
async fn the_clients_a_person_sees_a_terminal_through_are_listed() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut asker = d.attach("qoop-agents").await;
    assert!(
        clients(&mut asker, terminal).await.is_empty(),
        "a program's connection is nobody"
    );

    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    assert!(
        clients(&mut asker, terminal).await.is_empty(),
        "the terminal an interface is handed by default is not one it shows"
    );
    watch(&mut ui, terminal, true).await;
    assert_eq!(clients(&mut asker, terminal).await, ["computer"]);

    let attach = d.attach(qoop_runtime::client::ATTACH_ID).await;
    assert_eq!(clients(&mut asker, terminal).await, ["attach", "computer"]);

    let mut phone = d.attach_phone().await;
    watch(&mut phone, terminal, true).await;
    assert_eq!(
        clients(&mut asker, terminal).await,
        ["attach", "computer", "phone"]
    );

    watch(&mut ui, terminal, false).await;
    assert_eq!(clients(&mut asker, terminal).await, ["attach", "phone"]);

    drop(attach);
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while clients(&mut asker, terminal).await != ["phone"] {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a connection that ended is still listed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn the_kind_a_person_last_typed_from_is_kept() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut asker = d.attach("qoop-agents").await;
    let mut send = d.attach("qoop-send").await;
    send.input_to(terminal, b"from a program\r").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(row(&mut asker, terminal).await.typed_by, None);

    let mut phone = d.attach_phone().await;
    phone.input_to(terminal, b"y").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let typed = row(&mut asker, terminal).await.typed_by.expect("typed");
    assert_eq!(typed.kind, "phone");

    let mut attach = d.attach(qoop_runtime::client::ATTACH_ID).await;
    attach.input_to(terminal, b"n").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let typed = row(&mut asker, terminal).await.typed_by.expect("typed");
    assert_eq!(typed.kind, "attach", "qoop attach is a person typing");
}
