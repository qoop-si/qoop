//! The phone's preview of a dev server on the Mac: a shown `http://localhost:<port>` opened
//! as tunnels whose bytes ride the Blob channel of the phone's own connection.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use qoop_proto::{Channel, Frame, Reassembler};
use qoop_runtime::client;
use qoop_runtime::control::{BLOB_MOST, ControlRequest, ControlResponse};
use qoop_testkit::TestDaemon;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::mpsc;

const PATIENCE: Duration = Duration::from_secs(10);

/// A raw client that speaks Blob, the way the phone does.
struct Phone {
    stream: UnixStream,
    inbuf: BytesMut,
    seq: u32,
    control: Reassembler,
    answers: Vec<ControlResponse>,
    bytes: HashMap<u32, Vec<u8>>,
    closed: HashSet<u32>,
}

impl Phone {
    async fn connect(d: &TestDaemon) -> Self {
        let (stream, inbuf) = d.connect_phone().await;
        Phone {
            stream,
            inbuf,
            seq: 0,
            control: Reassembler::new(),
            answers: Vec::new(),
            bytes: HashMap::new(),
            closed: HashSet::new(),
        }
    }

    async fn ask(&mut self, request: &ControlRequest) {
        let out = client::encode_control_seq(request, &mut self.seq).expect("encode");
        self.stream.write_all(&out).await.expect("write");
    }

    async fn frame(&mut self, frame: Frame) {
        let mut out = BytesMut::new();
        frame.encode(&mut out).expect("encode");
        self.stream.write_all(&out).await.expect("write");
    }

    /// `bytes` to the dev server behind `conn`, in whole frames; empty is that side's end.
    async fn send(&mut self, conn: u32, bytes: &[u8]) {
        if bytes.is_empty() {
            return self
                .frame(Frame::new(Channel::Blob, conn, Bytes::new()))
                .await;
        }
        for piece in bytes.chunks(BLOB_MOST) {
            let body = Bytes::copy_from_slice(piece);
            self.frame(Frame::new(Channel::Blob, conn, body)).await;
        }
    }

    /// Read until `done` holds, or `PATIENCE` runs out.
    async fn until(&mut self, done: impl Fn(&Self) -> bool) -> bool {
        let by = Instant::now() + PATIENCE;
        let mut read = vec![0u8; 64 * 1024];
        loop {
            while let Some(frame) = Frame::decode(&mut self.inbuf).expect("decode") {
                match frame.channel {
                    Channel::Blob if frame.body.is_empty() => {
                        self.closed.insert(frame.stream);
                    }
                    Channel::Blob => {
                        let got = self.bytes.entry(frame.stream).or_default();
                        got.extend_from_slice(&frame.body);
                    }
                    Channel::Control => {
                        if let Ok(Some(message)) = self.control.push(frame, Instant::now()) {
                            self.answers
                                .push(serde_json::from_slice(&message.body).expect("answer"));
                        }
                    }
                    _ => {}
                }
            }
            if done(self) {
                return true;
            }
            let left = by.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.stream.read(&mut read)).await {
                Ok(Ok(n)) if n > 0 => self.inbuf.extend_from_slice(&read[..n]),
                _ => return false,
            }
        }
    }

    async fn answer(&mut self, id: &str) -> ControlResponse {
        assert!(
            self.until(|p| p.answers.iter().any(|a| a.request_id() == id))
                .await,
            "no answer to {id}"
        );
        let at = self.answers.iter().position(|a| a.request_id() == id);
        self.answers.remove(at.unwrap())
    }

    async fn ask_open(&mut self, show: u64, conn: u32) -> String {
        let id = format!("open-{conn}");
        let request = ControlRequest::TunnelOpen {
            id: id.clone(),
            show,
            conn,
        };
        self.ask(&request).await;
        id
    }

    async fn open(&mut self, show: u64, conn: u32) -> ControlResponse {
        let id = self.ask_open(show, conn).await;
        self.answer(&id).await
    }

    fn got(&self, conn: u32) -> usize {
        self.bytes.get(&conn).map_or(0, Vec::len)
    }
}

/// A server on the Mac's loopback that says `hi` first and echoes; each connection's end is
/// told on the receiver.
async fn echo() -> (u16, mpsc::UnboundedReceiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (ended, ends) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let ended = ended.clone();
            tokio::spawn(async move {
                tcp.write_all(b"hi\n").await.expect("hi");
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => tcp.write_all(&buf[..n]).await.expect("echo"),
                    }
                }
                let _ = tcp.shutdown().await;
                let _ = ended.send(());
            });
        }
    });
    (port, ends)
}

/// An HTTP server that answers every request with `body`, then closes.
async fn http(body: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = tcp.read(&mut buf).await.expect("read");
                    assert!(n > 0, "the request ended early");
                    head.extend_from_slice(&buf[..n]);
                }
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                tcp.write_all(answer.as_bytes()).await.expect("head");
                tcp.write_all(&body).await.expect("body");
            });
        }
    });
    port
}

/// Show `target` from the daemon's first terminal, as its agent would.
async fn show(d: &TestDaemon, target: String) -> ControlResponse {
    let terminal = d.terminals().streams()[0];
    let token = d.agents_arc().token_of(terminal).expect("a token");
    let mut agent = d.attach("qoop-show").await;
    let request = ControlRequest::ShowWrite {
        id: "show".into(),
        terminal,
        token: token.as_str().to_string(),
        target,
        note: None,
    };
    agent.control(&request, PATIENCE).await.expect("an answer")
}

async fn shown(d: &TestDaemon, port: u16) -> u64 {
    match show(d, format!("http://localhost:{port}/")).await {
        ControlResponse::ShowWritten { show, .. } => show,
        other => panic!("not shown: {other:?}"),
    }
}

#[tokio::test]
async fn a_tunnel_carries_bytes_both_ways_and_each_side_ends() {
    let d = TestDaemon::start().await;
    let (port, mut ends) = echo().await;
    let show = shown(&d, port).await;
    let mut phone = Phone::connect(&d).await;
    assert_eq!(
        phone.open(show, 1).await,
        ControlResponse::TunnelOpened {
            id: "open-1".into(),
            conn: 1
        }
    );
    // What the server says before it is asked anything arrives.
    assert!(phone.until(|p| p.got(1) >= 3).await);
    let sent: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    phone.send(1, &sent).await;
    let want = [&b"hi\n"[..], &sent].concat();
    assert!(phone.until(|p| p.got(1) >= want.len()).await);
    assert_eq!(phone.bytes[&1], want);
    // The phone's end reaches the server, and the server's end reaches the phone.
    phone.send(1, &[]).await;
    assert!(tokio::time::timeout(PATIENCE, ends.recv()).await.is_ok());
    assert!(phone.until(|p| p.closed.contains(&1)).await);
}

#[tokio::test]
async fn a_page_comes_through_and_an_answer_is_not_held_behind_it() {
    let d = TestDaemon::start().await;
    let body: Vec<u8> = (0..5_000_000u32).map(|i| (i % 253) as u8).collect();
    let port = http(body.clone()).await;
    let show = shown(&d, port).await;
    let mut phone = Phone::connect(&d).await;
    assert!(matches!(
        phone.open(show, 2).await,
        ControlResponse::TunnelOpened { conn: 2, .. }
    ));
    phone
        .send(2, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await;
    assert!(phone.until(|p| p.got(2) > 0).await);
    // Unread for a while, so the burst stands queued on the Mac when the question comes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = phone.got(2);
    phone
        .ask(&ControlRequest::ListTerminals { id: "list".into() })
        .await;
    assert!(matches!(
        phone.answer("list").await,
        ControlResponse::Terminals { .. }
    ));
    let behind = phone.got(2) - before;
    assert!(
        behind < 1_000_000,
        "{behind} bytes of the page came before the answer"
    );
    assert!(phone.until(|p| p.closed.contains(&2)).await);
    let got = &phone.bytes[&2];
    assert!(got.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(got.ends_with(&body), "the page arrived changed");
}

#[tokio::test]
async fn only_a_shown_loopback_port_is_dialed() {
    let d = TestDaemon::start().await;
    // A dev server is shown in the one form the phone opens.
    match show(&d, "http://0.0.0.0:3000".into()).await {
        ControlResponse::Failed { reason, .. } => {
            assert_eq!(reason, "show it as http://localhost:3000");
        }
        other => panic!("0.0.0.0 was taken: {other:?}"),
    }
    let public = match show(&d, "https://example.com/".into()).await {
        ControlResponse::ShowWritten { show, .. } => show,
        other => panic!("{other:?}"),
    };
    // Shown, but nothing listens there any more.
    let quiet = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = quiet.local_addr().expect("addr").port();
    drop(quiet);
    let stopped = shown(&d, port).await;

    let mut phone = Phone::connect(&d).await;
    for (show, conn) in [(public, 1), (9999, 2), (stopped, 3)] {
        let answer = phone.open(show, conn).await;
        assert!(
            matches!(answer, ControlResponse::Failed { .. }),
            "{answer:?}"
        );
    }
    assert_eq!(
        phone.open(stopped, 4).await,
        ControlResponse::Failed {
            id: "open-4".into(),
            reason: format!("nothing is listening on port {port}"),
        }
    );
}

#[tokio::test]
async fn a_conn_opens_once_closes_alone_and_ends_with_its_connection() {
    let d = TestDaemon::start().await;
    let (port, mut ends) = echo().await;
    let show = shown(&d, port).await;
    let mut phone = Phone::connect(&d).await;
    for conn in [5, 6] {
        assert!(matches!(
            phone.open(show, conn).await,
            ControlResponse::TunnelOpened { .. }
        ));
    }
    assert!(matches!(
        phone.open(show, 5).await,
        ControlResponse::Failed { .. }
    ));
    // A frame that is not whole closes its own tunnel, and that one only.
    let mut torn = Frame::new(Channel::Blob, 6, Bytes::from_static(b"x"));
    torn.flags = qoop_proto::Flags::default();
    phone.frame(torn).await;
    assert!(tokio::time::timeout(PATIENCE, ends.recv()).await.is_ok());
    assert!(phone.until(|p| p.closed.contains(&6)).await);
    phone.send(5, b"still\n").await;
    assert!(phone.until(|p| p.got(5) >= 9).await);
    assert_eq!(phone.bytes[&5], b"hi\nstill\n");
    // The phone goes, and its tunnel with it.
    drop(phone);
    assert!(tokio::time::timeout(PATIENCE, ends.recv()).await.is_ok());
}

#[tokio::test]
async fn bytes_sent_before_the_answer_reach_the_server() {
    let d = TestDaemon::start().await;
    let (port, _ends) = echo().await;
    let show = shown(&d, port).await;
    let mut phone = Phone::connect(&d).await;
    let id = phone.ask_open(show, 1).await;
    phone.send(1, b"early\n").await;
    assert!(matches!(
        phone.answer(&id).await,
        ControlResponse::TunnelOpened { conn: 1, .. }
    ));
    assert!(phone.until(|p| p.got(1) >= 9).await);
    assert_eq!(phone.bytes[&1], b"hi\nearly\n");
}

#[tokio::test]
async fn a_server_that_stops_reading_closes_only_its_tunnel() {
    let d = TestDaemon::start().await;
    // Accepts and never reads: the phone's frames pile up on the Mac.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            held.push(tcp);
        }
    });
    let show = shown(&d, port).await;
    let mut phone = Phone::connect(&d).await;
    assert!(matches!(
        phone.open(show, 3).await,
        ControlResponse::TunnelOpened { .. }
    ));
    let piece = vec![7u8; BLOB_MOST];
    let mut sent = 0;
    while !phone.closed.contains(&3) {
        assert!(sent < 2000, "64 frames queued and more never closed it");
        phone.send(3, &piece).await;
        sent += 1;
        if sent % 16 == 0 {
            let closed = phone.until(|p| p.closed.contains(&3));
            let _ = tokio::time::timeout(Duration::from_millis(20), closed).await;
        }
    }
    assert!(sent > 64, "closed after {sent} frames");
    // The connection itself goes on.
    let list = ControlRequest::ListTerminals { id: "list".into() };
    phone.ask(&list).await;
    assert!(matches!(
        phone.answer("list").await,
        ControlResponse::Terminals { .. }
    ));
}

#[tokio::test]
async fn only_the_phone_opens_a_tunnel_or_a_path_or_reads_changes() {
    let d = TestDaemon::start().await;
    let (port, _ends) = echo().await;
    let show = shown(&d, port).await;
    let terminal = d.terminals().streams()[0];
    let mut local = d.attach("qoop-local").await;
    for request in [
        ControlRequest::TunnelOpen {
            id: "t".into(),
            show,
            conn: 1,
        },
        ControlRequest::Changes {
            id: "c".into(),
            terminal,
        },
        ControlRequest::ShowOpen {
            id: "o".into(),
            terminal,
            target: "/etc/hosts".into(),
            auto: false,
        },
    ] {
        let id = request.request_id().to_string();
        assert_eq!(
            local.control(&request, PATIENCE).await,
            Some(ControlResponse::Failed {
                id,
                reason: "only the phone app asks for that".into(),
            })
        );
    }
}

/// A url tapped in a chat is the phone's own show, and opens a tunnel like an agent's.
#[tokio::test]
async fn a_url_tapped_in_a_chat_opens_a_tunnel() {
    let d = TestDaemon::start().await;
    let (port, _ends) = echo().await;
    let mut phone = Phone::connect(&d).await;
    phone
        .ask(&ControlRequest::ShowOpen {
            id: "tap".into(),
            terminal: d.terminals().streams()[0],
            target: format!("http://localhost:{port}/"),
            auto: false,
        })
        .await;
    let ControlResponse::Shows { shows, .. } = phone.answer("tap").await else {
        panic!("not opened");
    };
    assert_eq!(shows[0].kind, "url");
    assert!(matches!(
        phone.open(shows[0].show, 1).await,
        ControlResponse::TunnelOpened { conn: 1, .. }
    ));
    assert!(phone.until(|p| p.got(1) >= 3).await);
}

#[tokio::test]
async fn a_torn_tunnel_is_told_even_behind_another_ones_burst() {
    let d = TestDaemon::start().await;
    let (port, _ends) = echo().await;
    let quiet = shown(&d, port).await;
    let body: Vec<u8> = vec![9u8; 5_000_000];
    let busy = shown(&d, http(body.clone()).await).await;
    let mut phone = Phone::connect(&d).await;
    for (show, conn) in [(quiet, 1), (busy, 2)] {
        assert!(matches!(
            phone.open(show, conn).await,
            ControlResponse::TunnelOpened { .. }
        ));
    }
    phone
        .send(2, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await;
    assert!(phone.until(|p| p.got(2) > 0).await);
    // Unread, so the burst fills the lane; then tunnel 1 is torn.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut torn = Frame::new(Channel::Blob, 1, Bytes::from_static(b"x"));
    torn.flags = qoop_proto::Flags::default();
    phone.frame(torn).await;
    assert!(phone.until(|p| p.closed.contains(&2)).await);
    assert!(
        phone.until(|p| p.closed.contains(&1)).await,
        "the torn tunnel's end was lost"
    );
}
