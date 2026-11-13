//! `AgentRows`: the rows a client names, each as `ListAgents` writes it, and nothing else. What
//! a phone reads on `AgentChanged` instead of every row.

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn rows(client: &mut TestClient, request: ControlRequest) -> Vec<AgentRow> {
    match client.control(&request, PATIENCE).await {
        Some(ControlResponse::Agents { agents, splits, .. }) => {
            if matches!(request, ControlRequest::AgentRows { .. }) {
                assert!(splits.is_empty(), "no split trees for some rows");
            }
            agents
        }
        other => panic!("no rows: {other:?}"),
    }
}

#[tokio::test]
async fn only_the_named_rows_come_back_as_the_index_writes_them() {
    let d = TestDaemon::start().await;
    let mut client = d.attach("phone").await;
    let first = d.terminals().streams()[0];
    let created = client
        .control(
            &ControlRequest::CreateTerminal {
                id: "tab".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    let Some(ControlResponse::TerminalCreated {
        terminal: second, ..
    }) = created
    else {
        panic!("no terminal: {created:?}");
    };
    let all = rows(&mut client, ControlRequest::ListAgents { id: "all".into() }).await;
    assert!(all.len() >= 2, "{all:?}");

    let some = rows(
        &mut client,
        ControlRequest::AgentRows {
            id: "some".into(),
            terminals: vec![second, 999_999],
        },
    )
    .await;
    assert_eq!(some.len(), 1, "the one named and alive: {some:?}");
    let row = &some[0];
    assert_eq!(row.terminal, second);
    let listed = all.iter().find(|r| r.terminal == second).expect("listed");
    assert_eq!(
        serde_json::to_value(row).expect("row"),
        serde_json::to_value(listed).expect("listed row"),
        "the same row as the index"
    );
    assert!(
        !some.iter().any(|r| r.terminal == first),
        "a row not named is left out"
    );
    let none = rows(
        &mut client,
        ControlRequest::AgentRows {
            id: "none".into(),
            terminals: Vec::new(),
        },
    )
    .await;
    assert!(none.is_empty());
}
