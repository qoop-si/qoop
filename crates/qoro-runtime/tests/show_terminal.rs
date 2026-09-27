//! `Focus`: every attached interface is told to show a terminal, and whoever asked learns
//! which processes those interfaces are, so a notch can raise the window one runs in.

use std::time::{Duration, Instant};

use qoro_runtime::client::{self, Incoming};
use qoro_runtime::control::{ControlRequest, ControlResponse, EventMessage};
use qoro_testkit::TestDaemon;
use tokio::io::AsyncReadExt;

const PATIENCE: Duration = Duration::from_secs(5);

/// Read an interface's event lane until it is told to show a terminal.
async fn shown(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut bytes::BytesMut,
) -> Option<u32> {
    let mut connection = client::Connection::new();
    let deadline = Instant::now() + PATIENCE;
    let mut read = [0u8; 64 * 1024];
    loop {
        for incoming in connection.drain(inbuf).expect("decode") {
            if let Incoming::Event(EventMessage::Focus { terminal }) = incoming {
                return Some(terminal);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, rd.read(&mut read)).await {
            Ok(Ok(n)) if n > 0 => inbuf.extend_from_slice(&read[..n]),
            _ => return None,
        }
    }
}

#[tokio::test]
async fn an_interface_is_told_and_the_asker_learns_its_process() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let (ui, mut ui_in) = client::connect(d.socket(), "qoro-tui")
        .await
        .expect("connect");
    let (mut ui_rd, _ui_wr) = ui.into_split();

    let mut asker = d.attach("qoro-focus").await;
    let answer = asker
        .control(
            &ControlRequest::Focus {
                id: "show-1".into(),
                terminal,
            },
            PATIENCE,
        )
        .await;
    // The interface is this test process, and the asker is not an interface: one pid.
    assert_eq!(
        answer,
        Some(ControlResponse::Focused {
            id: "show-1".into(),
            clients: vec![std::process::id()],
        })
    );
    assert_eq!(shown(&mut ui_rd, &mut ui_in).await, Some(terminal));
}

#[tokio::test]
async fn a_terminal_that_is_not_there_is_refused() {
    let d = TestDaemon::start().await;
    let mut asker = d.attach("qoro-focus").await;
    let answer = asker
        .control(
            &ControlRequest::Focus {
                id: "show-2".into(),
                terminal: 9999,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(answer, Some(ControlResponse::Failed { .. })),
        "{answer:?}"
    );
}
