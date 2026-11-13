//! A desktop window is an interface like the TUI: it says `qoro-tui` with the `term` hello
//! key and asks for row deltas. It is told which rows each screen changed, the program's
//! title and its cursor, and its keys count as a person's (`PersonAt`).

use std::time::{Duration, Instant};

use qoop_runtime::client::{self, Connection, Incoming};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_runtime::daemon::{ClientWire, SnapshotWire};
use qoop_testkit::{PATIENCE, TestDaemon};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn a_window_asking_for_deltas_gets_rows_title_and_cursor_and_is_a_person() {
    client::set_runs_in("other", None);
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let (stream, mut inbuf) = client::connect_deltas(d.socket(), qoop_proto::INTERFACE_ID)
        .await
        .expect("connect");
    let (mut rd, mut wr) = stream.into_split();
    let mut window = Connection::new().deltas();

    let mut seq = 0;
    let line = b"printf '\\033]2;gui-title\\007\\033[6 q'; echo MARK_$((6*7))\r";
    let typed = client::encode_command_to(
        terminal,
        &ClientWire::Input {
            bytes: line.to_vec(),
            from_terminal: None,
            from_token: None,
        },
        &mut seq,
    )
    .expect("encode");
    wr.write_all(&typed).await.expect("type");

    let mut renders: Vec<(SnapshotWire, Option<Vec<usize>>)> = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    let mut read = [0u8; 64 * 1024];
    let done = |renders: &[(SnapshotWire, Option<Vec<usize>>)]| {
        renders.iter().any(|(s, _)| {
            s.title == "gui-title"
                && s.modes.cursor_style == 6
                && s.lines.iter().any(|l| l == "MARK_42")
        })
    };
    while !done(&renders) && Instant::now() < deadline {
        for incoming in window.drain(&mut inbuf).expect("drain") {
            if let Incoming::Render {
                terminal: t,
                snapshot,
                changed,
            } = incoming
                && t == terminal
            {
                renders.push((snapshot, changed));
            }
        }
        if let Some(ack) = window.renders().due_ack(Instant::now()) {
            wr.write_all(&ack).await.expect("ack");
        }
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(100), rd.read(&mut read)).await
        {
            assert!(n > 0, "the daemon ended the connection");
            inbuf.extend_from_slice(&read[..n]);
        }
    }
    assert!(
        done(&renders),
        "no screen with the title, the bar and the echo"
    );
    assert_eq!(renders[0].1, None, "the first screen is whole");
    let (screen, changed) = renders
        .iter()
        .find(|(_, changed)| changed.as_ref().is_some_and(|rows| !rows.is_empty()))
        .expect("a delta naming its rows");
    let rows = changed.as_ref().unwrap();
    assert!(rows.len() < screen.rows, "a delta names some rows, not all");
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "in order: {rows:?}");

    let mut asker = d.attach("qoop-agents").await;
    let until = match asker
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
    };
    assert!(
        until.is_some(),
        "the window typed, and a window is a person"
    );
}
