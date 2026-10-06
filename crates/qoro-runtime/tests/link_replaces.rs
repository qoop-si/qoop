//! A link that reconnects ends the connection it replaces, and the size moves with it.
//!
//! Over ssh a dropped network leaves the far `qoro relay` holding its socket: the remote sshd
//! has not noticed, so the daemon never sees the close ADR-007 moves a size on. The interface
//! reconnects, and every size it asks for was refused until the no-ack sweep, 30 seconds
//! later: a split in a remote space left the old pane cut at its old width.

use std::time::{Duration, Instant};

use qoro_testkit::{PATIENCE, TestClient, TestDaemon};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// A handshaken connection that says which link it is, or nothing.
async fn linked(d: &TestDaemon, key: Option<&str>) -> UnixStream {
    let stream = UnixStream::connect(d.socket()).await.expect("connect");
    let (stream, _, _) = qoro_runtime::client::handshake(stream, "qoro-tui", key)
        .await
        .expect("handshake");
    stream
}

/// A link's first connection: the size owner of terminal 1 at 132x43, after which it reads
/// and writes nothing, as a relay behind a dead ssh does.
async fn stale_owner(d: &TestDaemon, key: Option<&str>) -> UnixStream {
    let mut stream = linked(d, key).await;
    let mut seq = 0u32;
    let resize = qoro_runtime::client::encode_command_to(
        1,
        &qoro_runtime::daemon::ClientWire::Resize {
            cols: 132,
            rows: 43,
        },
        &mut seq,
    )
    .expect("encode");
    stream.write_all(&resize).await.expect("write");
    // A witness that never asks for a size, so it owns nothing, sees the size land.
    let mut witness = d.attach("witness").await;
    assert!(
        sees_size(&mut witness, "43 132", PATIENCE).await,
        "the first connection never became the size owner, so this proves nothing"
    );
    stream
}

/// Ask the shell its size until it says `size`: a size handed over when the old owner goes
/// lands a moment after the ask, never at a fixed time.
async fn sees_size(client: &mut TestClient, size: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        client.forget_seen();
        client.input(b"stty size\n").await;
        if client.wait_for_text(size, Duration::from_millis(500)).await {
            return true;
        }
    }
    false
}

/// Whether the daemon closed `stream` within `within`.
async fn closed(stream: &mut UnixStream, within: Duration) -> bool {
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), stream.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return true,
            _ => continue,
        }
    }
    false
}

/// The connection that takes over, asking for 90x30, as a split does.
async fn takes_size(d: &TestDaemon, key: Option<&str>) -> TestClient {
    let stream = linked(d, key).await;
    let mut next = TestClient::from_parts(stream, Default::default(), 1);
    next.resize(90, 30).await;
    next
}

#[tokio::test]
async fn a_reconnected_link_ends_its_old_connection_and_gets_the_size() {
    let d = TestDaemon::start().await;
    let mut old = stale_owner(&d, Some("link-a")).await;

    let mut next = takes_size(&d, Some("link-a")).await;
    assert!(
        closed(&mut old, Duration::from_secs(5)).await,
        "the link's old connection stayed open after the link reconnected"
    );
    assert!(
        sees_size(&mut next, "30 90", Duration::from_secs(5)).await,
        "the reconnected link's size was refused, so a split leaves the old pane cut off"
    );
}

#[tokio::test]
async fn another_link_or_none_ends_nothing() {
    let d = TestDaemon::start().await;
    let mut old = stale_owner(&d, Some("link-a")).await;

    let mut other = takes_size(&d, Some("link-b")).await;
    let mut keyless = linked(&d, None).await;
    assert!(
        !closed(&mut old, Duration::from_secs(2)).await,
        "a connection of another link ended this one"
    );
    // Asked well after the refused size reached the terminal: still the owner's.
    other.forget_seen();
    other.input(b"stty size\n").await;
    assert!(
        other.wait_for_text("43 132", PATIENCE).await,
        "another link took the size from a live owner (ADR-007)"
    );
    assert!(
        !closed(&mut keyless, Duration::from_millis(500)).await,
        "a connection that names no link was ended"
    );
}

/// Welcomes can land out of order: an attempt the link gave up on, answered late, must not
/// end the connection that replaced it.
#[tokio::test]
async fn a_late_welcome_does_not_end_the_newer_connection() {
    let d = TestDaemon::start().await;
    // Admitted first, its hello held back.
    let slow = UnixStream::connect(d.socket()).await.expect("connect");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut newer = linked(&d, Some("link-a")).await;
    let (mut slow, _, _) = qoro_runtime::client::handshake(slow, "qoro-tui", Some("link-a"))
        .await
        .expect("handshake");
    assert!(
        !closed(&mut newer, Duration::from_secs(2)).await,
        "an older attempt's late welcome ended the newer connection"
    );
    assert!(
        !closed(&mut slow, Duration::from_millis(500)).await,
        "the older attempt was ended by a connection admitted after it"
    );
}
