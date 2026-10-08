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
        "client_name": "qoro-mobile",
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
