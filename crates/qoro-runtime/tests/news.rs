//! `News`: the terminals an interface counts as news for its person are marked on every
//! row for as long as that interface is attached, and every index hears when that moves.

use std::time::{Duration, Instant};

use qoro_runtime::client::{self, Incoming};
use qoro_runtime::control::{ControlRequest, ControlResponse, EventMessage};
use qoro_testkit::{TestClient, TestDaemon};
use tokio::io::AsyncReadExt;

const PATIENCE: Duration = Duration::from_secs(5);

/// Whether the index says `terminal` is news.
async fn news(asker: &mut TestClient, terminal: u32) -> bool {
    match asker
        .control(&ControlRequest::ListAgents { id: "rows".into() }, PATIENCE)
        .await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .iter()
            .any(|row| row.terminal == terminal && row.news),
        other => panic!("no index: {other:?}"),
    }
}

/// Say which terminals are news, from `ui`.
async fn say(ui: &mut TestClient, terminals: Vec<u32>) {
    let answer = ui
        .control(
            &ControlRequest::News {
                id: "news".into(),
                terminals,
            },
            PATIENCE,
        )
        .await;
    assert_eq!(
        answer,
        Some(ControlResponse::NewsNoted { id: "news".into() })
    );
}

/// Read an index's event lane until it hears that `terminal`'s row changed.
async fn changed(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
    terminal: u32,
) -> bool {
    let mut connection = client::Connection::new();
    let deadline = Instant::now() + PATIENCE;
    let mut read = [0u8; 64 * 1024];
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

#[tokio::test]
async fn news_is_on_the_row_while_its_interface_is_attached() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let (index, mut index_in) = client::connect(d.socket(), "qoro-watch")
        .await
        .expect("connect");
    let (mut index_rd, _index_wr) = index.into_split();
    let mut asker = d.attach("qoro-agents").await;
    assert!(!news(&mut asker, terminal).await, "nothing said yet");

    let mut ui = d.attach("qoro-tui").await;
    say(&mut ui, vec![terminal]).await;
    assert!(
        changed(&mut index_rd, &mut index_in, terminal).await,
        "an index was not told the row moved"
    );
    assert!(news(&mut asker, terminal).await);

    // Said again without it: looked at, news no more.
    say(&mut ui, Vec::new()).await;
    assert!(changed(&mut index_rd, &mut index_in, terminal).await);
    assert!(!news(&mut asker, terminal).await);

    // Held for the interface's life only: gone with it, and the index told.
    say(&mut ui, vec![terminal]).await;
    assert!(changed(&mut index_rd, &mut index_in, terminal).await);
    drop(ui);
    assert!(changed(&mut index_rd, &mut index_in, terminal).await);
    let deadline = Instant::now() + PATIENCE;
    while news(&mut asker, terminal).await {
        assert!(Instant::now() < deadline, "news outlived its interface");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
