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
    let (index, mut inbuf) = client::connect(d.socket(), "qoro-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoro-task").await;
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
    let mut c = d.attach("qoro-task").await;
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
    let (index, mut inbuf) = client::connect(d.socket(), "qoro-watch")
        .await
        .expect("connect");
    let (mut rd, _wr) = index.into_split();
    let mut c = d.attach("qoro-task").await;
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
            task
        }
    );
    assert!(
        !changed(&mut rd, &mut inbuf, terminal, Duration::from_millis(1500)).await,
        "a begin on an open task changes nothing"
    );
}
