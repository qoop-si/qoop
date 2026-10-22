//! `SetQassSkip`: the spaces and tabs the owner keeps from qass are marked on the rows every
//! client and the watcher read, announced when they move, refused for qass's own space, and
//! forgotten with the tab.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use qoop_runtime::client::{self, Incoming};
use qoop_runtime::control::{
    AgentRow, ControlRequest, ControlResponse, EventMessage, WorkspaceRow,
};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};
use tokio::io::AsyncReadExt;

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE)
        .await
        .expect("an answer to the request")
}

async fn new_space(c: &mut TestClient, name: &str) -> i64 {
    match ask(
        c,
        ControlRequest::CreateWorkspace {
            id: format!("space-{name}"),
            name: name.into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a created workspace, got {other:?}"),
    }
}

async fn new_tab(c: &mut TestClient, workspace: i64, id: &str) -> u32 {
    match ask(
        c,
        ControlRequest::CreateTerminal {
            id: id.into(),
            cwd: None,
            workspace: Some(workspace),
            name: None,
        },
    )
    .await
    {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

async fn rows(c: &mut TestClient) -> Vec<AgentRow> {
    match ask(c, ControlRequest::ListAgents { id: "rows".into() }).await {
        ControlResponse::Agents { agents, .. } => agents,
        other => panic!("expected the agents, got {other:?}"),
    }
}

async fn spaces(c: &mut TestClient) -> Vec<WorkspaceRow> {
    match ask(c, ControlRequest::ListWorkspaces { id: "list".into() }).await {
        ControlResponse::Workspaces { workspaces, .. } => workspaces,
        other => panic!("expected the spaces, got {other:?}"),
    }
}

async fn skip(
    c: &mut TestClient,
    workspace: i64,
    terminal: Option<u32>,
    on: bool,
) -> ControlResponse {
    ask(
        c,
        ControlRequest::SetQassSkip {
            id: format!("skip-{workspace}-{terminal:?}-{on}"),
            workspace,
            terminal,
            on,
        },
    )
    .await
}

/// Whether `terminal`'s row says qass leaves it alone.
async fn skipped(c: &mut TestClient, terminal: u32) -> bool {
    Box::pin(rows(c))
        .await
        .iter()
        .find(|r| r.terminal == terminal)
        .is_some_and(|r| r.qass_skip)
}

/// Read an index's event lane until it has heard that every terminal in `want` changed.
async fn heard(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    want: &BTreeSet<u32>,
) -> bool {
    let mut connection = client::Connection::new();
    let mut seen = BTreeSet::new();
    let deadline = Instant::now() + PATIENCE;
    let mut read = [0u8; 64 * 1024];
    loop {
        for incoming in connection.drain(inbuf).expect("decode") {
            if let Incoming::Event(EventMessage::AgentChanged { terminal }) = incoming {
                seen.insert(terminal);
            }
        }
        if want.is_subset(&seen) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rd.read(&mut read)).await {
            Ok(Ok(n)) if n > 0 => inbuf.extend_from_slice(&read[..n]),
            _ => return false,
        }
    }
}

/// A space turned on marks every tab in it, on the tab's own row, and tells every index; the
/// space's row carries its own flag. Turned off, all of it goes. A tab of another space is
/// never touched.
#[tokio::test]
async fn a_space_skip_marks_every_tab_in_it_and_announces_them() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("qoop-agents").await;
    let work = new_space(&mut c, "work").await;
    let other = new_space(&mut c, "other").await;
    let (a, b) = (
        new_tab(&mut c, work, "t-a").await,
        new_tab(&mut c, work, "t-b").await,
    );
    let outside = new_tab(&mut c, other, "t-c").await;
    let (index, mut index_in) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut index_rd, _index_wr) = index.into_split();

    assert!(matches!(
        skip(&mut c, work, None, true).await,
        ControlResponse::Renamed { .. }
    ));
    assert!(
        Box::pin(heard(&mut index_rd, &mut index_in, &BTreeSet::from([a, b]))).await,
        "an index was not told every tab of the space moved"
    );
    assert!(skipped(&mut c, a).await && skipped(&mut c, b).await);
    assert!(!skipped(&mut c, outside).await, "another space was marked");
    let listed = spaces(&mut c).await;
    assert!(listed.iter().find(|s| s.id == work).unwrap().qass_skip);
    assert!(!listed.iter().find(|s| s.id == other).unwrap().qass_skip);

    skip(&mut c, work, None, false).await;
    assert!(!skipped(&mut c, a).await && !skipped(&mut c, b).await);
    assert!(!spaces(&mut c).await.iter().any(|s| s.qass_skip));
}

/// A pane turned on marks only that tab's row, and its space's row stays as it was.
#[tokio::test]
async fn a_tab_skip_marks_only_that_row() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("qoop-agents").await;
    let work = new_space(&mut c, "work").await;
    let (a, b) = (
        new_tab(&mut c, work, "t-a").await,
        new_tab(&mut c, work, "t-b").await,
    );
    let (index, mut index_in) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut index_rd, _index_wr) = index.into_split();

    assert!(matches!(
        skip(&mut c, work, Some(a), true).await,
        ControlResponse::Renamed { .. }
    ));
    assert!(
        Box::pin(heard(&mut index_rd, &mut index_in, &BTreeSet::from([a]))).await,
        "an index was not told the row moved"
    );
    assert!(skipped(&mut c, a).await);
    assert!(!skipped(&mut c, b).await, "a sibling was marked");
    assert!(!spaces(&mut c).await.iter().any(|s| s.qass_skip));
    assert!(matches!(
        skip(&mut c, work, Some(9999), true).await,
        ControlResponse::Failed { .. }
    ));
}

/// qass is never skipped from where it lives: its space and its tab are refused, whichever way
/// the request names them, and nothing is marked.
#[tokio::test]
async fn qass_s_own_space_and_tab_are_refused() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("qoop-agents").await;
    let qass = new_space(&mut c, "qass").await;
    let tab = new_tab(&mut c, qass, "t-q").await;
    for terminal in [None, Some(tab)] {
        let answer = skip(&mut c, qass, terminal, true).await;
        assert!(
            matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("qass")),
            "{answer:?}"
        );
    }
    assert!(!skipped(&mut c, tab).await);
    assert!(!spaces(&mut c).await.iter().any(|s| s.qass_skip));
    assert!(d.qass_skipped_tabs().is_empty());
}

/// A shell that ends on its own takes its mark with it: its number may be given to a new tab,
/// which must start watched.
#[tokio::test]
async fn a_shell_that_exits_on_its_own_loses_its_skip() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("qoop-agents").await;
    let work = new_space(&mut c, "work").await;
    let tab = new_tab(&mut c, work, "t-a").await;
    skip(&mut c, work, Some(tab), true).await;
    assert_eq!(d.qass_skipped_tabs(), vec![tab]);

    c.input_to(tab, b"exit\r").await;
    let deadline = Instant::now() + PATIENCE;
    while !d.qass_skipped_tabs().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the mark outlived the shell: {:?}",
            d.qass_skipped_tabs()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!rows(&mut c).await.iter().any(|r| r.terminal == tab));
}
