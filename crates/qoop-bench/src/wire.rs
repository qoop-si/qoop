//! A thin client over the real protocol, built directly on `qoop_runtime::client`.
//!
//! `qoop-testkit`'s `TestClient` cannot be reused here: it is only ever constructed by a
//! `TestDaemon`, which runs the daemon in-process. The memory scenario needs a real `qoopd`
//! subprocess so its resident size can be read from the OS, so this talks to that
//! subprocess's socket directly. The decode and framing logic is not duplicated: both this
//! and `TestClient` sit on top of `qoop_runtime::client`'s `connect`, `encode_command_seq`
//! and `RenderStream`, so a harness cannot drift from what the real client does.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use qoop_runtime::client::{self, RenderStream};
use qoop_runtime::daemon::{ClientWire, SnapshotWire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

pub struct BenchClient {
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
    inbuf: BytesMut,
    seq: u32,
    renders: RenderStream,
}

impl BenchClient {
    pub async fn connect(socket: &Path, id: &str) -> io::Result<Self> {
        let (stream, inbuf) = client::connect(socket, id).await?;
        let (rd, wr) = stream.into_split();
        Ok(Self {
            rd,
            wr,
            inbuf,
            seq: 0,
            renders: RenderStream::new(),
        })
    }

    pub async fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.send(&ClientWire::Input {
            bytes: bytes.to_vec(),
            from_terminal: None,
            from_token: None,
        })
        .await
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.send(&ClientWire::Resize { cols, rows }).await
    }

    async fn send(&mut self, cmd: &ClientWire) -> io::Result<()> {
        let buf = client::encode_command_seq(cmd, &mut self.seq)?;
        self.wr.write_all(&buf).await
    }

    /// Wait for a screen satisfying `pred`, the same reassembly a real client uses. Returns
    /// `None` on timeout rather than hanging, so a caller reports what it expected.
    pub async fn wait_for(
        &mut self,
        pred: impl Fn(&SnapshotWire) -> bool,
        within: Duration,
    ) -> Option<SnapshotWire> {
        let deadline = Instant::now() + within;
        if let Some(s) = self.drain_buffered(&pred) {
            return Some(s);
        }
        let mut buf = [0u8; 64 * 1024];
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let n = match tokio::time::timeout(remaining, self.rd.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => n,
                Ok(Err(_)) => break,
            };
            self.inbuf.extend_from_slice(&buf[..n]);
            if let Some(s) = self.drain_buffered(&pred) {
                return Some(s);
            }
        }
        None
    }

    fn drain_buffered(&mut self, pred: &impl Fn(&SnapshotWire) -> bool) -> Option<SnapshotWire> {
        let mut hit = None;
        while let Some(s) = self.renders.take_latest(&mut self.inbuf).expect("decode") {
            if pred(&s) {
                hit = Some(s);
            }
        }
        hit
    }
}
