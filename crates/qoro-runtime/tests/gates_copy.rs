//! Clipboard writes from a child, end to end.
//!
//! A program that selects text itself (an agent that took the mouse) copies with OSC 52
//! when it has no clipboard tool, which is every program on a remote over ssh. The daemon
//! must hand that text to the clients watching the terminal, once, and never to a client
//! that arrives after the copy was made.

use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoro_runtime::client::{self, Connection, Incoming};
use qoro_testkit::{PATIENCE, TestDaemon};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;

/// Everything a watcher is sent within `within`, decoded the way the interface does.
async fn read_for(
    sock: &mut UnixStream,
    inbuf: &mut BytesMut,
    conn: &mut Connection,
    within: Duration,
    mut done: impl FnMut(&[Incoming]) -> bool,
) -> Vec<Incoming> {
    let deadline = Instant::now() + within;
    let mut got = conn.drain(inbuf).expect("drain");
    let mut buf = [0u8; 64 * 1024];
    while !done(&got) && Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, sock.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                inbuf.extend_from_slice(&buf[..n]);
                got.extend(conn.drain(inbuf).expect("drain"));
            }
            _ => break,
        }
    }
    got
}

fn copies(got: &[Incoming]) -> Vec<(u32, String)> {
    got.iter()
        .filter_map(|m| match m {
            Incoming::Copy { terminal, text } => Some((*terminal, text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_childs_clipboard_write_reaches_a_watcher_once_and_never_a_late_one() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("typist").await;
    let stream = c.default_stream();
    let (mut sock, mut inbuf) = client::connect(d.socket(), "watcher")
        .await
        .expect("connect");
    let mut conn = Connection::new();
    // Subscribed by default to the lowest terminal: its first screen says so.
    let first = read_for(&mut sock, &mut inbuf, &mut conn, PATIENCE, |g| {
        g.iter().any(|m| matches!(m, Incoming::Render { .. }))
    })
    .await;
    assert!(
        first
            .iter()
            .any(|m| matches!(m, Incoming::Render { terminal, .. } if *terminal == stream)),
        "the watcher never saw terminal {stream}"
    );

    // dash printf: `\033` is ESC and `\007` is BEL. "copied text" in base64.
    c.input(b"printf '\\033]52;c;Y29waWVkIHRleHQ=\\007'; echo DONE_$((6*7))\n")
        .await;
    assert!(c.wait_for_text("DONE_42", PATIENCE).await);
    let got = read_for(&mut sock, &mut inbuf, &mut conn, PATIENCE, |g| {
        !copies(g).is_empty()
    })
    .await;
    assert_eq!(copies(&got), vec![(stream, "copied text".to_string())]);

    // More output wakes the subscription again; the copy is not sent a second time.
    c.input(b"echo AGAIN_$((6*7))\n").await;
    assert!(c.wait_for_text("AGAIN_42", PATIENCE).await);
    let more = read_for(
        &mut sock,
        &mut inbuf,
        &mut conn,
        Duration::from_secs(1),
        |_| false,
    )
    .await;
    assert!(copies(&more).is_empty(), "sent twice: {:?}", copies(&more));

    // A watcher that arrives after the copy gets screens, never the old copy.
    let (mut late, mut late_in) = client::connect(d.socket(), "late").await.expect("connect");
    let mut late_conn = Connection::new();
    c.input(b"echo LATE_$((6*7))\n").await;
    let seen = read_for(
        &mut late,
        &mut late_in,
        &mut late_conn,
        Duration::from_secs(2),
        |_| false,
    )
    .await;
    assert!(
        seen.iter().any(|m| matches!(m, Incoming::Render { .. })),
        "the late watcher saw nothing at all"
    );
    assert!(copies(&seen).is_empty(), "replayed: {:?}", copies(&seen));
}

/// A program that copies and exits at once: the end of the terminal must not overtake
/// the copy on its way out.
#[tokio::test]
async fn a_copy_made_just_before_the_child_exits_still_arrives() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("typist").await;
    let stream = c.default_stream();
    let (mut sock, mut inbuf) = client::connect(d.socket(), "watcher")
        .await
        .expect("connect");
    let mut conn = Connection::new();
    read_for(&mut sock, &mut inbuf, &mut conn, PATIENCE, |g| {
        g.iter().any(|m| matches!(m, Incoming::Render { .. }))
    })
    .await;
    // "last words" in base64, then the shell ends the terminal.
    c.input(b"printf '\\033]52;c;bGFzdCB3b3Jkcw==\\007'; exit\n")
        .await;
    let got = read_for(&mut sock, &mut inbuf, &mut conn, PATIENCE, |g| {
        !copies(g).is_empty()
    })
    .await;
    assert_eq!(copies(&got), vec![(stream, "last words".to_string())]);
}
