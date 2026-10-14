//! A program in a tab borrows the microphone of the interface a person types into: refused
//! while no such interface is attached, heard from it once one is, one recording at a
//! time, and the interface told when it ends. What qoop's `arecord` relies on to give
//! Claude Code's voice a microphone on a machine that has none.

use std::time::Duration;

use base64::Engine as _;
use bytes::BytesMut;
use qoop_proto::{Channel, Frame, Reassembler};
use qoop_runtime::client;
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage};
use qoop_runtime::daemon::ClientWire;
use qoop_testkit::{PATIENCE, TestDaemon};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// A connection read frame by frame, controls and events both, which `TestClient` drops.
struct Raw {
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    buf: BytesMut,
    controls: Reassembler,
    seq: u32,
    terminal_seq: u32,
}

enum Got {
    Control(ControlResponse),
    Event(EventMessage),
}

impl Raw {
    async fn connect(d: &TestDaemon, id: &str) -> Raw {
        let (stream, buf) = client::connect(d.socket(), id).await.expect("connect");
        let (rd, wr) = stream.into_split();
        Raw {
            rd,
            wr,
            buf,
            controls: Reassembler::new(),
            seq: 0,
            terminal_seq: 0,
        }
    }

    async fn ask(&mut self, request: &ControlRequest) {
        let out = client::encode_control_seq(request, &mut self.seq).expect("encode");
        self.wr.write_all(&out).await.expect("write");
    }

    async fn audio(&mut self, terminal: u32, bytes: &[u8]) {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let out = client::encode_command_to(
            terminal,
            &ClientWire::Audio { data },
            &mut self.terminal_seq,
        )
        .expect("encode");
        self.wr.write_all(&out).await.expect("write");
    }

    /// The next control or event that `pick` keeps, within `PATIENCE`.
    async fn next<T>(&mut self, pick: impl Fn(Got) -> Option<T>) -> Option<T> {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        let mut read = [0u8; 64 * 1024];
        loop {
            while let Some(frame) = Frame::decode(&mut self.buf).expect("decode") {
                let got = match frame.channel {
                    Channel::Control => self
                        .controls
                        .push(frame, std::time::Instant::now())
                        .ok()
                        .flatten()
                        .and_then(|m| serde_json::from_slice(&m.body).ok())
                        .map(Got::Control),
                    Channel::Event => serde_json::from_slice(&frame.body).ok().map(Got::Event),
                    _ => None,
                };
                if let Some(kept) = got.and_then(&pick) {
                    return Some(kept);
                }
            }
            let n = tokio::time::timeout_at(deadline, self.rd.read(&mut read))
                .await
                .ok()?
                .ok()?;
            if n == 0 {
                return None;
            }
            self.buf.extend_from_slice(&read[..n]);
        }
    }

    async fn answer(&mut self, id: &str) -> ControlResponse {
        self.next(|got| match got {
            Got::Control(r) if r.request_id() == id => Some(r),
            _ => None,
        })
        .await
        .unwrap_or_else(|| panic!("no answer to {id}"))
    }

    async fn listen_event(&mut self) -> (u32, u64, bool) {
        self.next(|got| match got {
            Got::Event(EventMessage::Listen {
                terminal,
                listener,
                on,
            }) => Some((terminal, listener, on)),
            _ => None,
        })
        .await
        .expect("no listen event")
    }
}

fn listen(d: &TestDaemon, id: &str, terminal: u32, probe: bool) -> ControlRequest {
    ControlRequest::Listen {
        id: id.into(),
        terminal,
        probe,
        token: d
            .agents_arc()
            .token_of(terminal)
            .expect("a token")
            .as_str()
            .to_string(),
    }
}

#[tokio::test]
async fn a_recording_hears_the_interface_and_ends_when_its_reader_goes() {
    let d = TestDaemon::start().await;
    let terminal = d.attach("first").await.default_stream();
    let mut shim = Raw::connect(&d, "arecord").await;

    // Nobody lends a microphone: refused at once, so Claude Code says voice is unavailable
    // rather than recording silence.
    shim.ask(&listen(&d, "p1", terminal, true)).await;
    assert!(matches!(
        shim.answer("p1").await,
        ControlResponse::Failed { .. }
    ));

    let mut window = Raw::connect(&d, qoop_proto::INTERFACE_ID).await;
    // Only a program in that tab, which holds its token, may ask.
    let mut stranger = listen(&d, "p0", terminal, true);
    if let ControlRequest::Listen { token, .. } = &mut stranger {
        *token = "not-it".into();
    }
    shim.ask(&stranger).await;
    assert!(matches!(
        shim.answer("p0").await,
        ControlResponse::Failed { .. }
    ));
    shim.ask(&listen(&d, "p2", terminal, true)).await;
    assert!(matches!(
        shim.answer("p2").await,
        ControlResponse::Listening { .. }
    ));
    // A probe starts nothing.
    shim.ask(&listen(&d, "r1", terminal, false)).await;
    assert!(matches!(
        shim.answer("r1").await,
        ControlResponse::Listening { .. }
    ));
    let (said, listener, on) = window.listen_event().await;
    assert_eq!((said, on), (terminal, true));

    // Said again while it lasts, so a missed one never leaves a microphone on.
    let (_, again, on) = window.listen_event().await;
    assert_eq!((again, on), (listener, true));

    window.audio(terminal, b"pcm").await;
    let heard = shim
        .next(|got| match got {
            Got::Control(ControlResponse::Heard { id, data }) if id == "r1" => Some(data),
            _ => None,
        })
        .await
        .expect("nothing heard");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(heard)
            .unwrap(),
        b"pcm"
    );

    // No takeover while it records, but a probe is still answered.
    let mut other = Raw::connect(&d, "arecord-2").await;
    other.ask(&listen(&d, "r2", terminal, false)).await;
    assert!(matches!(
        other.answer("r2").await,
        ControlResponse::Failed { .. }
    ));
    other.ask(&listen(&d, "p3", terminal, true)).await;
    assert!(matches!(
        other.answer("p3").await,
        ControlResponse::Listening { .. }
    ));

    // The reader goes: the window is told, and the microphone is free again.
    drop(shim);
    let off = loop {
        let (_, l, on) = window.listen_event().await;
        if !on {
            break l;
        }
    };
    assert_eq!(off, listener);
    tokio::time::sleep(Duration::from_millis(50)).await;
    other.ask(&listen(&d, "r3", terminal, false)).await;
    assert!(matches!(
        other.answer("r3").await,
        ControlResponse::Listening { .. }
    ));
}

/// Two windows on the same tab both lending: only the first one heard is passed on, so
/// the two never mix into one recording.
#[tokio::test]
async fn only_the_first_window_heard_is_passed_on() {
    let d = TestDaemon::start().await;
    let terminal = d.attach("first").await.default_stream();
    let mut a = Raw::connect(&d, qoop_proto::INTERFACE_ID).await;
    let mut b = Raw::connect(&d, qoop_proto::INTERFACE_ID).await;
    let mut shim = Raw::connect(&d, "arecord").await;
    shim.ask(&listen(&d, "r1", terminal, false)).await;
    assert!(matches!(
        shim.answer("r1").await,
        ControlResponse::Listening { .. }
    ));
    a.listen_event().await;
    a.audio(terminal, b"a1").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    b.audio(terminal, b"b1").await;
    a.audio(terminal, b"a2").await;
    let mut heard = Vec::new();
    while heard.len() < 2 {
        let data = shim
            .next(|got| match got {
                Got::Control(ControlResponse::Heard { data, .. }) => Some(data),
                _ => None,
            })
            .await
            .expect("heard");
        heard.push(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
        );
    }
    assert_eq!(heard, vec![b"a1".to_vec(), b"a2".to_vec()]);
}
