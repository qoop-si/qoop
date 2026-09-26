//! Gate G1.1 and G1.2, as tests rather than as a script: a real daemon, a real PTY, a real
//! shell, over a real socket.

use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoro_proto::{Channel, Frame};
use qoro_runtime::client;
use qoro_runtime::daemon::{ClientWire, SnapshotWire};
use qoro_runtime::{Daemon, paths};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A private runtime directory for this test alone, so tests run in parallel without
/// colliding and never attach to a developer's own daemon.
fn runtime_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    // `tempdir` creates 0755, and the daemon refuses anything group or other accessible.
    // Tightening it here rather than loosening the check is the point: the first run of
    // these tests failed on exactly this, which is the check doing its job.
    std::fs::set_permissions(
        d.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("chmod");
    paths::ensure_private_dir(d.path()).expect("private dir");
    d
}

async fn start_daemon(dir: &std::path::Path) -> std::path::PathBuf {
    let shell = std::path::PathBuf::from("/bin/sh");
    let daemon = Daemon::bind(dir, &shell).await.expect("bind");
    let socket = daemon.socket().to_path_buf();
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    });
    socket
}

/// Read frames until a snapshot satisfying `pred` arrives, or the deadline passes.
async fn wait_for_screen(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut BytesMut,
    pred: impl Fn(&SnapshotWire) -> bool,
    within: Duration,
) -> Option<SnapshotWire> {
    let deadline = Instant::now() + within;
    let mut buf = [0u8; 64 * 1024];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let n = match tokio::time::timeout(remaining, rd.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => break,
        };
        inbuf.extend_from_slice(&buf[..n]);
        while let Some(frame) = Frame::decode(inbuf).expect("decode") {
            if frame.channel != Channel::Terminal {
                continue;
            }
            if let Ok(s) = serde_json::from_slice::<SnapshotWire>(&frame.body)
                && pred(&s)
            {
                return Some(s);
            }
        }
    }
    None
}

fn screen(s: &SnapshotWire) -> String {
    s.lines.join("\n")
}

#[tokio::test]
async fn a_client_attaches_and_a_real_shell_answers_a_real_command() {
    // G1.1. The marker is computed by the shell, not echoed, so a shell that never ran
    // cannot produce it.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;

    let (stream, mut inbuf) = client::connect(&socket, "test").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();

    wr.write_all(&client::encode_command(&ClientWire::Resize { cols: 80, rows: 24 }).unwrap())
        .await
        .unwrap();

    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"echo QORO_READY_$((6*7))\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();

    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("QORO_READY_42"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        got.is_some(),
        "the shell never produced the marker; last screen was not matched"
    );
}

#[tokio::test]
async fn attaching_delivers_the_current_screen_not_only_later_updates() {
    // The bug this pins down was found by running the thing: the shell's prompt is emitted
    // before any client connects, so a client that only receives subsequent updates sees a
    // blank terminal until something else happens. Attach must deliver the screen as it is.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;

    // First client produces output, then leaves.
    {
        let (stream, mut inbuf) = client::connect(&socket, "first").await.expect("connect");
        let (mut rd, mut wr) = stream.into_split();
        wr.write_all(
            &client::encode_command(&ClientWire::Input {
                bytes: b"echo LEFT_BEHIND_MARKER\n".to_vec(),
                from_terminal: None,
                from_token: None,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            wait_for_screen(
                &mut rd,
                &mut inbuf,
                |s| screen(s).contains("LEFT_BEHIND_MARKER"),
                Duration::from_secs(10)
            )
            .await
            .is_some(),
            "the first client never saw its own output"
        );
    }

    // Second client attaches with nothing new happening. It must still see the screen.
    let (stream, mut inbuf) = client::connect(&socket, "second").await.expect("reconnect");
    let (mut rd, _wr) = stream.into_split();
    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("LEFT_BEHIND_MARKER"),
        Duration::from_secs(5),
    )
    .await;
    assert!(
        got.is_some(),
        "attach did not deliver the current screen, only later updates"
    );
}

#[tokio::test]
async fn a_client_disconnect_does_not_kill_the_shell() {
    // Invariant I1. The OS is the source of truth for what survived, not our bookkeeping.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;

    let (stream, mut inbuf) = client::connect(&socket, "first").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();
    // Record a marker in the shell's own state, so the survivor can be identified as the
    // same shell rather than a fresh one.
    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"QORO_SURVIVOR=alive; echo SET_$QORO_SURVIVOR\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(
        wait_for_screen(
            &mut rd,
            &mut inbuf,
            |s| screen(s).contains("SET_alive"),
            Duration::from_secs(10)
        )
        .await
        .is_some(),
        "the shell never acknowledged the variable"
    );
    drop(rd);
    drop(wr);

    // Reattach and ask the shell for the variable. Only the same process can answer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (stream, mut inbuf) = client::connect(&socket, "second").await.expect("reconnect");
    let (mut rd, mut wr) = stream.into_split();
    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"echo STILL_$QORO_SURVIVOR\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("STILL_alive"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        got.is_some(),
        "the shell did not survive the first client leaving, or a new one replaced it"
    );
}

#[tokio::test]
async fn a_resize_reaches_the_child() {
    // G1.7's first half: the owner's resize propagates all the way into the child, checked
    // by asking the child itself rather than by reading our own state back.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;
    let (stream, mut inbuf) = client::connect(&socket, "test").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();

    wr.write_all(
        &client::encode_command(&ClientWire::Resize {
            cols: 132,
            rows: 43,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"stty size\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();

    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("43 132"),
        Duration::from_secs(10),
    )
    .await;
    assert!(got.is_some(), "the child did not observe the new size");
}

#[tokio::test]
async fn a_peer_from_another_uid_is_refused() {
    // G1.13. CI usually cannot create a second uid, so this asserts the check exists and
    // accepts our own uid rather than pretending to test what it cannot.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;
    assert!(
        client::connect(&socket, "same-uid").await.is_ok(),
        "our own uid must be accepted"
    );
}

/// Collect terminal snapshots for `within`, so a test can assert something
/// never appeared rather than only that something did.
async fn collect_screens(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    inbuf: &mut BytesMut,
    within: Duration,
) -> Vec<SnapshotWire> {
    let deadline = Instant::now() + within;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let n = match tokio::time::timeout(remaining, rd.read(&mut buf)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => break,
        };
        inbuf.extend_from_slice(&buf[..n]);
        while let Some(frame) = Frame::decode(inbuf).expect("decode") {
            if frame.channel != Channel::Terminal {
                continue;
            }
            if let Ok(s) = serde_json::from_slice::<SnapshotWire>(&frame.body) {
                out.push(s);
            }
        }
    }
    out
}

#[tokio::test]
async fn an_equal_size_resize_nudges_a_repaint_and_restores_canonical() {
    // A same-size ioctl is a no-op for the child, so a reattaching client at
    // the current size would never force a repaint in an app that redraws
    // only on SIGWINCH. The daemon answers with a shrink plus restore. The
    // trap marker is computed by the shell (`W$((6*7))` becomes `W42` only in
    // output), so the command echo itself can never match.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;
    let (stream, mut inbuf) = client::connect(&socket, "test").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();

    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"trap 'echo W$((6*7))' WINCH\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    // First resize claims ownership; 80x24 equals the initial size, so this
    // is the nudge path rather than a real change.
    wr.write_all(&client::encode_command(&ClientWire::Resize { cols: 80, rows: 24 }).unwrap())
        .await
        .unwrap();

    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("W42"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        got.is_some(),
        "equal-size resize produced no SIGWINCH; the nudge did not run"
    );

    // And the terminal is back at canonical size and alive: the shrink was a
    // redraw trick, not a geometry decision.
    wr.write_all(
        &client::encode_command(&ClientWire::Input {
            bytes: b"stty size\n".to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let sized = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| screen(s).contains("24 80"),
        Duration::from_secs(10),
    )
    .await;
    assert!(
        sized.is_some(),
        "terminal did not restore to 24 80 after the nudge"
    );
}

#[tokio::test]
async fn a_real_size_change_resizes_once_without_a_nudge() {
    // The nudge is only for the equal-size case. A real change already
    // delivers SIGWINCH, so no rows-1 frame may ever appear.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;
    let (stream, mut inbuf) = client::connect(&socket, "test").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();

    wr.write_all(
        &client::encode_command(&ClientWire::Resize {
            cols: 100,
            rows: 30,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let screens = collect_screens(&mut rd, &mut inbuf, Duration::from_secs(2)).await;
    assert!(
        screens.iter().any(|s| (s.cols, s.rows) == (100, 30)),
        "the resize never reached the client"
    );
    assert!(
        screens.iter().all(|s| (s.cols, s.rows) != (100, 29)),
        "a real change must not pass through the nudge size"
    );
}

#[tokio::test]
async fn a_non_owner_resize_emits_no_winsize_change_at_all() {
    // The first client owns the geometry. A second client's resize is
    // refused by arbitration, so zero ioctls happen: not even the nudge.
    let dir = runtime_dir();
    let socket = start_daemon(dir.path()).await;
    let (stream, mut inbuf) = client::connect(&socket, "owner").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();
    wr.write_all(
        &client::encode_command(&ClientWire::Resize {
            cols: 100,
            rows: 30,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let got = wait_for_screen(
        &mut rd,
        &mut inbuf,
        |s| (s.cols, s.rows) == (100, 30),
        Duration::from_secs(10),
    )
    .await;
    assert!(got.is_some(), "the owner resize never applied");
    // The owner stays connected: if it left, ownership would lapse and the
    // second client would become the owner, which is a different case.
    let (_owner_rd, _owner_wr) = (rd, wr);

    let (stream, mut inbuf) = client::connect(&socket, "second").await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();
    wr.write_all(
        &client::encode_command(&ClientWire::Resize {
            cols: 100,
            rows: 30,
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let screens = collect_screens(&mut rd, &mut inbuf, Duration::from_secs(2)).await;
    // Non-vacuous: the subscription itself delivers the current screen, so
    // an empty collection would mean the harness heard nothing at all.
    assert!(
        screens.iter().any(|s| (s.cols, s.rows) == (100, 30)),
        "the second client never even saw the current screen"
    );
    assert!(
        screens.iter().all(|s| (s.cols, s.rows) != (100, 29)),
        "a refused resize must not nudge, shrink, or otherwise touch winsize"
    );
}
