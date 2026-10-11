//! Task writes against an in-process daemon: which writes an index hears about, and that a
//! terminal's own token is what lets a write speak for it.

use std::time::{Duration, Instant};

use qoop_runtime::client::{self, Incoming};
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage, TaskOp, TaskOutcome};
use qoop_testkit::{TestClient, TestDaemon};
use tokio::io::AsyncReadExt;

const PATIENCE: Duration = Duration::from_secs(5);

/// Whether an index hears, within `wait`, that `terminal`'s row changed.
async fn changed(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    terminal: u32,
    wait: Duration,
) -> bool {
    let mut connection = client::Connection::new();
    let deadline = Instant::now() + wait;
    // On the heap: this future is awaited many times in one test, and a 64 KiB array in
    // each await point overflowed the test thread's stack in a debug build.
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

async fn write(
    c: &mut TestClient,
    terminal: Option<u32>,
    token: Option<String>,
    task: Option<i64>,
    action: TaskOp,
) -> ControlResponse {
    c.control(
        &ControlRequest::TaskWrite {
            id: "w".into(),
            terminal,
            token,
            task,
            action,
        },
        PATIENCE,
    )
    .await
    .expect("an answer")
}

#[tokio::test]
async fn an_index_hears_what_a_row_shows_and_nothing_else() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .agents_arc()
        .token_of(terminal)
        .expect("a token")
        .as_str()
        .to_string();
    let (index, mut inbuf) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoop-task").await;
    // Whatever the start of the terminal said, said before anything is asked.
    while changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(500)).await {}

    let me = (Some(terminal), Some(token.clone()));
    let opened = write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::New {
            goal: "g".into(),
            replace: false,
        },
    )
    .await;
    let ControlResponse::TaskWritten { task, .. } = opened else {
        panic!("not opened: {opened:?}")
    };
    assert!(
        changed(&mut rd, &mut inbuf, terminal, PATIENCE).await,
        "an open is heard"
    );

    let wrote = write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::Evidence {
            text: "check: ok".into(),
        },
    )
    .await;
    assert!(
        matches!(wrote, ControlResponse::TaskWritten { .. }),
        "{wrote:?}"
    );
    let wrote = write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::Plan {
            text: "plan".into(),
        },
    )
    .await;
    assert!(
        matches!(wrote, ControlResponse::TaskWritten { .. }),
        "{wrote:?}"
    );
    assert!(
        !changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(1500)).await,
        "evidence and a plan change nothing a row shows"
    );

    write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::Stage {
            stage: "investigating".into(),
        },
    )
    .await;
    assert!(
        !changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(1500)).await,
        "the same stage again, with no claim, is not news"
    );
    write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::Done {
            summary: String::new(),
        },
    )
    .await;
    assert!(
        changed(&mut rd, &mut inbuf, terminal, PATIENCE).await,
        "a claim is heard"
    );
    write(
        &mut c,
        me.0,
        me.1.clone(),
        None,
        TaskOp::Stage {
            stage: "investigating".into(),
        },
    )
    .await;
    assert!(
        changed(&mut rd, &mut inbuf, terminal, PATIENCE).await,
        "a cleared claim is heard"
    );

    // A close by id, from a client that is no terminal, is heard on the task's terminal.
    let closed = write(
        &mut c,
        None,
        None,
        Some(task),
        TaskOp::Close {
            outcome: TaskOutcome::Verified,
            reason: String::new(),
        },
    )
    .await;
    assert!(
        matches!(closed, ControlResponse::TaskWritten { .. }),
        "{closed:?}"
    );
    assert!(
        changed(&mut rd, &mut inbuf, terminal, PATIENCE).await,
        "a close is heard"
    );
}

#[tokio::test]
async fn only_the_terminals_own_token_speaks_for_it() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("qoop-task").await;
    let refused = write(
        &mut c,
        Some(terminal),
        Some("not-its-token".into()),
        None,
        TaskOp::New {
            goal: "g".into(),
            replace: false,
        },
    )
    .await;
    match refused {
        ControlResponse::Failed { reason, .. } => assert!(reason.contains("refused"), "{reason}"),
        other => panic!("a forged token opened a task: {other:?}"),
    }
    let nobody = write(
        &mut c,
        None,
        None,
        None,
        TaskOp::New {
            goal: "g".into(),
            replace: false,
        },
    )
    .await;
    assert!(
        matches!(nobody, ControlResponse::Failed { .. }),
        "{nobody:?}"
    );
}

#[tokio::test]
async fn a_begin_opens_once_and_is_heard_once() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .agents_arc()
        .token_of(terminal)
        .expect("a token")
        .as_str()
        .to_string();
    let (index, mut inbuf) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoop-task").await;
    while changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(500)).await {}
    let me = (Some(terminal), Some(token));
    let first = write(&mut c, me.0, me.1.clone(), None, TaskOp::Begin).await;
    let ControlResponse::TaskWritten { task, .. } = first else {
        panic!("not begun: {first:?}")
    };
    assert!(task > 0, "a task, not the quiet answer");
    assert!(
        changed(&mut rd, &mut inbuf, terminal, PATIENCE).await,
        "an opening is heard"
    );
    let again = write(&mut c, me.0, me.1.clone(), None, TaskOp::Begin).await;
    assert_eq!(
        again,
        ControlResponse::TaskWritten {
            id: "w".into(),
            task,
            key: None
        }
    );
    assert!(
        !changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(1500)).await,
        "a begin on an open task changes nothing"
    );
}

/// Whether a client hears, within `wait`, that the board changed.
async fn board_changed(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    wait: Duration,
) -> bool {
    let mut connection = client::Connection::new();
    let deadline = Instant::now() + wait;
    let mut read = vec![0u8; 64 * 1024];
    loop {
        for incoming in connection.drain(inbuf).expect("decode") {
            if matches!(incoming, Incoming::Event(EventMessage::TasksChanged)) {
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

async fn board(c: &mut TestClient, board: bool) -> Option<Vec<qoop_runtime::control::ItemInfo>> {
    match c
        .control(
            &ControlRequest::TaskRead {
                id: "r".into(),
                board,
                task: None,
                terminal: None,
                token: None,
                before: None,
            },
            PATIENCE,
        )
        .await
        .expect("an answer")
    {
        ControlResponse::Tasks { items, .. } => items,
        other => panic!("not a board: {other:?}"),
    }
}

/// The board is read through the daemon, only when asked for; an add, a remote agent's
/// report and a space goal are heard as board news, and a write that changes nothing the
/// board shows (evidence, the same report again) is not.
#[tokio::test]
async fn the_board_is_read_on_request_and_its_changes_are_heard() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let workspace = d.terminals().workspace_of(terminal).expect("a space");
    let token = d
        .agents_arc()
        .token_of(terminal)
        .expect("a token")
        .as_str()
        .to_string();
    let (index, mut inbuf) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoop-task").await;
    while board_changed(&mut rd, &mut inbuf, Duration::from_millis(300)).await {}

    let added = write(
        &mut c,
        None,
        None,
        None,
        TaskOp::Add {
            workspace,
            goal: "search works".into(),
            description: Some("why".into()),
        },
    )
    .await;
    let ControlResponse::TaskWritten {
        task: item,
        key: Some(key),
        ..
    } = added
    else {
        panic!("not added: {added:?}")
    };
    assert!(key.ends_with("-1"), "{key}");
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "an add is heard"
    );
    assert_eq!(board(&mut c, false).await, None, "not asked for, not sent");
    let items = board(&mut c, true).await.expect("asked for, sent");
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].id, items[0].state.as_str()), (item, "todo"));

    let opened = write(
        &mut c,
        Some(terminal),
        Some(token.clone()),
        None,
        TaskOp::New {
            goal: "g".into(),
            replace: false,
        },
    )
    .await;
    assert!(
        matches!(opened, ControlResponse::TaskWritten { .. }),
        "{opened:?}"
    );
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "an open is heard"
    );
    write(
        &mut c,
        Some(terminal),
        Some(token),
        None,
        TaskOp::Evidence {
            text: "check: ok".into(),
        },
    )
    .await;
    assert!(
        !board_changed(&mut rd, &mut inbuf, Duration::from_millis(1500)).await,
        "evidence changes nothing the board shows"
    );

    let remote = || TaskOp::LinkRemote {
        item,
        remote: "box".into(),
        task: 4,
        name: "T2".into(),
        stage: "implementing".into(),
    };
    let reported = write(&mut c, None, None, None, remote()).await;
    assert!(
        matches!(reported, ControlResponse::TaskWritten { .. }),
        "{reported:?}"
    );
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "a remote agent is heard"
    );
    write(&mut c, None, None, None, remote()).await;
    assert!(
        !board_changed(&mut rd, &mut inbuf, Duration::from_millis(1500)).await,
        "the same report again is not news"
    );
    let items = board(&mut c, true).await.expect("a board");
    assert_eq!(items[0].state, "doing");
    let agent = &items[0].agents[0];
    assert_eq!((agent.machine.as_str(), agent.name.as_str()), ("box", "T2"));

    let goal = c
        .control(
            &ControlRequest::SetSpaceGoal {
                id: "g".into(),
                workspace,
                goal: "ship it".into(),
            },
            PATIENCE,
        )
        .await
        .expect("an answer");
    assert!(matches!(goal, ControlResponse::Renamed { .. }), "{goal:?}");
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "a space goal is heard"
    );
}

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE).await.expect("an answer")
}

async fn add(c: &mut TestClient, workspace: i64, goal: &str) -> i64 {
    let added = write(
        c,
        None,
        None,
        None,
        TaskOp::Add {
            workspace,
            goal: goal.into(),
            description: None,
        },
    )
    .await;
    let ControlResponse::TaskWritten { task, .. } = added else {
        panic!("not added: {added:?}")
    };
    task
}

/// Removing a space closes its open items, and closing a tab closes its task, which moves the
/// item it was on back to Todo: both are board news.
#[tokio::test]
async fn a_space_removed_and_a_tab_closed_are_board_news() {
    let d = TestDaemon::start().await;
    let workspace = d.workspace();
    let (index, mut inbuf) = client::connect(d.socket(), "qoop-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoop-task").await;

    let ControlResponse::WorkspaceCreated {
        workspace: spare, ..
    } = ask(
        &mut c,
        ControlRequest::CreateWorkspace {
            id: "s".into(),
            name: "spare".into(),
        },
    )
    .await
    else {
        panic!("no space")
    };
    add(&mut c, spare, "left behind").await;
    while board_changed(&mut rd, &mut inbuf, Duration::from_millis(300)).await {}
    let closed = ask(
        &mut c,
        ControlRequest::CloseWorkspace {
            id: "c".into(),
            workspace: spare,
        },
    )
    .await;
    assert!(
        matches!(closed, ControlResponse::WorkspaceClosed { .. }),
        "{closed:?}"
    );
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "a space removed is heard"
    );
    assert_eq!(board(&mut c, true).await, Some(Vec::new()), "its item went");

    let item = add(&mut c, workspace, "worked in a tab").await;
    let ControlResponse::TerminalCreated { terminal, .. } = ask(
        &mut c,
        ControlRequest::CreateTerminal {
            id: "t".into(),
            cwd: None,
            workspace: Some(workspace),
            name: None,
        },
    )
    .await
    else {
        panic!("no tab")
    };
    let token = d
        .agents_arc()
        .token_of(terminal)
        .expect("a token")
        .as_str()
        .to_string();
    for action in [
        TaskOp::New {
            goal: "g".into(),
            replace: false,
        },
        TaskOp::Link { item },
    ] {
        let wrote = write(&mut c, Some(terminal), Some(token.clone()), None, action).await;
        assert!(
            matches!(wrote, ControlResponse::TaskWritten { .. }),
            "{wrote:?}"
        );
    }
    assert_eq!(
        board(&mut c, true).await.expect("a board")[0].state,
        "doing"
    );
    while board_changed(&mut rd, &mut inbuf, Duration::from_millis(300)).await {}
    let closed = ask(
        &mut c,
        ControlRequest::CloseTerminal {
            id: "x".into(),
            terminal,
        },
    )
    .await;
    assert!(
        matches!(closed, ControlResponse::TerminalClosed { .. }),
        "{closed:?}"
    );
    assert!(
        board_changed(&mut rd, &mut inbuf, PATIENCE).await,
        "a tab closed is heard"
    );
    // Its only agent ended with the tab: the item stays in Doing, its agent finished, for
    // qass's check; it never falls back to Todo.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let item = board(&mut c, true).await.expect("a board").remove(0);
        if item.agents.iter().all(|a| a.closed) {
            assert_eq!(item.state, "doing", "a finished item stays in Doing");
            break;
        }
        assert!(Instant::now() < deadline, "its agent never read as ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
