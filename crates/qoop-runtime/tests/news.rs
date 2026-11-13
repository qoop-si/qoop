//! One status per row, decided by the daemon: a turn end nobody has looked at reads `done`
//! for every client alike until a person's interface says it was seen, a task waiting on the
//! owner reads `blocked` the moment its stage moves, and a sub-agent still running is work.

use std::time::{Duration, Instant};

use qoop_runtime::client::{self, Incoming};
use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse, EventMessage, TaskOp};
use qoop_testkit::{TestClient, TestDaemon};
use tokio::io::AsyncReadExt;

const PATIENCE: Duration = Duration::from_secs(5);

/// The index's row for `terminal`.
async fn row(asker: &mut TestClient, terminal: u32) -> AgentRow {
    // Boxed, here and in each helper: a control call holds a 64 KiB read buffer, and a test
    // that awaits many of them inline outgrew a debug build's 2 MiB test stack.
    match Box::pin(asker.control(&ControlRequest::ListAgents { id: "rows".into() }, PATIENCE)).await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|row| row.terminal == terminal)
            .expect("a row for the terminal"),
        other => panic!("no index: {other:?}"),
    }
}

/// The row's status token.
async fn status(asker: &mut TestClient, terminal: u32) -> String {
    row(asker, terminal).await.status.expect("a status")
}

/// What a hook in the terminal reports.
async fn report(hook: &mut TestClient, terminal: u32, token: &str, state: &str, event: &str) {
    let answer = Box::pin(hook.control(
        &ControlRequest::ReportAgentState {
            from: None,
            id: format!("{state}-{event}"),
            terminal,
            token: token.into(),
            state: state.into(),
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
            clock: 0,
            session: None,
            event: Some(event.into()),
            label: None,
            said: None,
        },
        PATIENCE,
    ))
    .await;
    assert!(
        !matches!(answer, Some(ControlResponse::Failed { .. }) | None),
        "the report was refused: {answer:?}"
    );
}

async fn seen(client: &mut TestClient, terminal: u32, stop: Option<String>) -> ControlResponse {
    Box::pin(client.control(
        &ControlRequest::Seen {
            id: "seen".into(),
            terminal,
            stop,
        },
        PATIENCE,
    ))
    .await
    .expect("an answer")
}

/// Read an index's event lane until it hears that `terminal`'s row changed.
async fn changed(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    terminal: u32,
) -> bool {
    changed_within(rd, inbuf, terminal, PATIENCE).await
}

/// [`changed`] within `patience`.
async fn changed_within(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    terminal: u32,
    patience: Duration,
) -> bool {
    let mut connection = client::Connection::new();
    let deadline = Instant::now() + patience;
    let mut read = vec![0u8; 64 * 1024];
    loop {
        for incoming in connection.drain(inbuf).expect("decode") {
            if let Incoming::Event(EventMessage::AgentChanged { terminal: t }) = incoming
                && t == terminal
            {
                return true;
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rd.read(&mut read)).await {
            Ok(Ok(n)) if n > 0 => inbuf.extend_from_slice(&read[..n]),
            _ => return false,
        }
    }
}

fn token_of(d: &TestDaemon, terminal: u32) -> String {
    d.terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string()
}

#[tokio::test]
async fn a_finish_reads_done_everywhere_until_a_person_sees_it() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut hook = d.attach("hook").await;
    let mut asker = d.attach("qoop-agents").await;

    report(&mut hook, terminal, &token, "working", "user").await;
    assert_eq!(status(&mut asker, terminal).await, "working");
    report(&mut hook, terminal, &token, "idle", "done").await;
    let done = row(&mut asker, terminal).await;
    assert_eq!(done.status.as_deref(), Some("done"));
    assert_eq!(
        done.state, "idle",
        "the raw state scripts wait on is unchanged"
    );
    assert!(done.news, "the notch's field says the same done");

    // A program cannot look; a look at another finish clears nothing.
    assert!(matches!(
        seen(&mut asker, terminal, None).await,
        ControlResponse::Failed { .. }
    ));
    let mut ui = d.attach(qoop_proto::INTERFACE_ID).await;
    seen(&mut ui, terminal, Some("1.1".into())).await;
    assert_eq!(status(&mut asker, terminal).await, "done");

    // The interface in front of the person says it was seen: every index hears it.
    let (index, mut index_in) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut index_rd, _index_wr) = index.into_split();
    assert_eq!(
        seen(&mut ui, terminal, done.stop.clone()).await,
        ControlResponse::NewsNoted { id: "seen".into() }
    );
    assert!(changed(&mut index_rd, &mut index_in, terminal).await);
    let looked = row(&mut asker, terminal).await;
    assert_eq!(looked.status.as_deref(), Some("idle"));
    assert!(!looked.news);

    // The next turn's end is a new finish nobody has seen.
    report(&mut hook, terminal, &token, "working", "user").await;
    report(&mut hook, terminal, &token, "idle", "done").await;
    assert_eq!(status(&mut asker, terminal).await, "done");
}

#[tokio::test]
async fn a_task_waiting_on_the_owner_needs_you_at_once_on_every_row() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    let mut hook = d.attach("hook").await;
    let mut asker = d.attach("qoop-agents").await;
    report(&mut hook, terminal, &token, "working", "user").await;
    report(&mut hook, terminal, &token, "idle", "done").await;
    let write = |id: &str, action| ControlRequest::TaskWrite {
        id: id.into(),
        terminal: Some(terminal),
        token: Some(token.clone()),
        task: None,
        action,
    };
    let wrote = hook.control(&write("new", TaskOp::Begin), PATIENCE).await;
    assert!(matches!(wrote, Some(ControlResponse::TaskWritten { .. })));

    let (index, mut index_in) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut index_rd, _index_wr) = index.into_split();
    // Whatever the task's opening announced is heard and done with first.
    while changed_within(
        &mut index_rd,
        &mut index_in,
        terminal,
        Duration::from_millis(500),
    )
    .await
    {}
    assert_eq!(status(&mut asker, terminal).await, "done");
    let stage = TaskOp::Stage {
        stage: "waiting-human".into(),
    };
    let wrote = hook.control(&write("wait", stage), PATIENCE).await;
    assert!(matches!(wrote, Some(ControlResponse::TaskWritten { .. })));
    // The stage moved and the agent did not: the row is announced all the same.
    assert!(
        changed(&mut index_rd, &mut index_in, terminal).await,
        "no index heard the question"
    );
    let waiting = row(&mut asker, terminal).await;
    assert_eq!(waiting.status.as_deref(), Some("blocked"));
    assert_eq!(waiting.state, "idle");
}

/// qass finishes a turn on every event it handles: its own tab never reads done.
#[tokio::test]
async fn qass_own_turns_are_never_done() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = token_of(&d, terminal);
    d.store()
        .rename_workspace(d.workspace(), "qass".into())
        .await
        .expect("qass's space");
    let mut hook = d.attach("hook").await;
    let mut asker = d.attach("qoop-agents").await;
    report(&mut hook, terminal, &token, "working", "user").await;
    report(&mut hook, terminal, &token, "idle", "done").await;
    assert_eq!(status(&mut asker, terminal).await, "idle");
}
