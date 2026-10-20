//! A client that asks for a render window gets no more than that many terminal messages
//! unacknowledged, and the next one after its ack is the newest screen, not an old one
//! queued behind it. What a phone behind a relay needs so a typed key's echo is not stuck
//! behind screens the relay is still carrying.

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use qoop_proto::{Channel, Frame};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A connection whose hello says `render_window: 1`, as the phone sends it.
async fn paced(d: &TestDaemon) -> TestClient {
    let mut stream = tokio::net::UnixStream::connect(d.socket()).await.unwrap();
    let hello = serde_json::json!({
        "codec_generation_min": qoop_proto::CODEC_GENERATION,
        "codec_generation_max": qoop_proto::CODEC_GENERATION,
        "client_id": "paced",
        "client_name": "qoop-mobile",
        "client_version": "0",
        "codecs": {
            "control": ["control.json.v1"],
            "event": ["event.json.v1"],
            "terminal": ["terminal.frame.v1"],
            "blob": ["blob.raw.v1"],
        },
        qoop_runtime::client::RENDER_WINDOW: 1,
    });
    let mut out = BytesMut::new();
    Frame::new(
        Channel::Handshake,
        0,
        Bytes::from(serde_json::to_vec(&hello).unwrap()),
    )
    .encode(&mut out)
    .unwrap();
    stream.write_all(&out).await.unwrap();
    let mut inbuf = BytesMut::new();
    let mut read = [0u8; 8192];
    loop {
        let n = stream.read(&mut read).await.unwrap();
        assert!(n > 0, "closed during the handshake");
        inbuf.extend_from_slice(&read[..n]);
        if let Some(frame) = Frame::decode(&mut inbuf).unwrap() {
            serde_json::from_slice::<qoop_proto::ServerWelcome>(&frame.body).expect("welcome");
            break;
        }
    }
    let first = d.terminals().streams().first().copied().unwrap_or(1);
    TestClient::from_parts(stream, inbuf, first)
}

#[tokio::test]
async fn a_paced_client_holds_one_screen_until_it_acks() {
    let d = TestDaemon::start().await;
    let mut driver = d.attach("driver").await;
    driver.input(b"echo READY_$((6*7))\n").await;
    assert!(driver.wait_for_text("READY_42", PATIENCE).await);

    let mut phone = paced(&d).await;
    // A flood: a new screen every few milliseconds for longer than the test reads.
    driver
        .input(b"for i in $(seq 1 400); do echo LINE_$i; sleep 0.01; done; echo DONE_$((6*7))\n")
        .await;

    // Read without acknowledging: one screen arrives, then nothing, however much changes.
    // The driver, unpaced and acknowledging, sees the flood meanwhile.
    let before = driver.renders_received();
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        phone.drain_incoming(Duration::from_millis(50)).await;
        driver.drain_incoming(Duration::from_millis(50)).await;
        driver.ack_if_due().await;
    }
    assert!(
        driver.renders_received() - before >= 10,
        "no flood reached the unpaced client, so the window was never tested"
    );
    assert_eq!(
        phone.renders_received(),
        1,
        "a client with a window of one took more than one screen without acknowledging"
    );

    // Acknowledge as the client does, and the screens come again, one per ack, the newest
    // each time: the last one names the end of the flood.
    let until = Instant::now() + Duration::from_secs(20);
    let mut last = String::new();
    while Instant::now() < until && !last.contains("DONE_42") {
        phone.drain_incoming(Duration::from_millis(100)).await;
        phone.ack_if_due().await;
        if let Some(s) = phone.screen(phone.default_stream()) {
            last = s.lines.join("\n");
        }
    }
    assert!(
        last.contains("DONE_42"),
        "the paced client never caught up to the newest screen"
    );
}

/// What an ssh link asks for: a window and row deltas. A one-row change crosses as a delta,
/// and what the link's reader passes on is the screen an unpaced client ends up with.
#[tokio::test]
async fn a_link_is_sent_deltas_and_passes_on_whole_screens() {
    use qoop_runtime::client::{Connection, Incoming, handshake_link};
    use qoop_runtime::daemon::{DELTA_PREFIX, SnapshotWire};

    let d = TestDaemon::start().await;
    let mut driver = d.attach("driver").await;
    driver.input(b"echo READY_$((6*7))\n").await;
    assert!(driver.wait_for_text("READY_42", PATIENCE).await);

    // A client that did not ask, as the phone and an older Mac: whole screens only.
    let plain = tokio::net::UnixStream::connect(d.socket()).await.unwrap();
    let (mut plain, mut plain_wire, _) = qoop_runtime::client::handshake(plain, "plain", None)
        .await
        .unwrap();
    let stream = tokio::net::UnixStream::connect(d.socket()).await.unwrap();
    let (stream, leftover, _) = handshake_link(stream, "link", "test-link").await.unwrap();
    let (mut rd, mut wr) = stream.into_split();
    // What the daemon wrote, kept to be read twice: as sent, and rebuilt.
    let mut wire = BytesMut::from(&leftover[..]);
    let mut paced = Connection::raw().paced();
    let mut inbuf = leftover;
    let mut typed = false;
    let until = Instant::now() + PATIENCE;
    let mut read = [0u8; 64 * 1024];
    while Instant::now() < until {
        if let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_millis(100), rd.read(&mut read)).await
        {
            assert!(n > 0, "the daemon closed the link");
            wire.extend_from_slice(&read[..n]);
            inbuf.extend_from_slice(&read[..n]);
        }
        paced.drain(&mut inbuf).unwrap();
        if let Some(ack) = paced.renders().due_ack(Instant::now()) {
            wr.write_all(&ack).await.unwrap();
        }
        if !typed && paced.renders().received() > 0 {
            driver.input(b"echo TYPED_$((6*7))\n").await;
            typed = true;
        }
        if typed
            && driver
                .wait_for_text("TYPED_42", Duration::from_millis(10))
                .await
        {
            break;
        }
    }
    // Quiet for a moment, so the last screen each side holds is the same one.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(200), rd.read(&mut read)).await
    {
        assert!(n > 0, "the daemon closed the link");
        wire.extend_from_slice(&read[..n]);
    }
    driver.drain_incoming(Duration::from_millis(200)).await;
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(100), plain.read(&mut read)).await
    {
        assert!(n > 0, "the daemon closed the plain client");
        plain_wire.extend_from_slice(&read[..n]);
    }
    let plain_bodies: Vec<_> = Connection::raw()
        .drain(&mut plain_wire)
        .unwrap()
        .into_iter()
        .filter_map(|incoming| match incoming {
            Incoming::Raw { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert!(!plain_bodies.is_empty(), "the plain client saw nothing");
    assert!(
        !plain_bodies.iter().any(|b| b.starts_with(DELTA_PREFIX)),
        "a client that did not ask was sent a delta"
    );

    let bodies: Vec<_> = Connection::raw()
        .drain(&mut wire.clone())
        .unwrap()
        .into_iter()
        .filter_map(|incoming| match incoming {
            Incoming::Raw { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert!(
        bodies.iter().any(|b| b.starts_with(DELTA_PREFIX)),
        "no delta crossed: {} messages",
        bodies.len()
    );
    let last: Option<SnapshotWire> = Connection::raw()
        .deltas()
        .drain(&mut wire)
        .unwrap()
        .into_iter()
        .filter_map(|incoming| match incoming {
            Incoming::Raw { body, .. } => serde_json::from_slice(&body).ok(),
            _ => None,
        })
        .last();
    let last = last.expect("no screen rebuilt");
    assert!(last.lines.join("\n").contains("TYPED_42"));
    assert_eq!(
        Some(&last),
        driver.screen(driver.default_stream()),
        "the rebuilt screen is not the one an unpaced client sees"
    );
}
