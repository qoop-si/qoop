//! A phone's preview of a dev server on this Mac (`qoop show http://localhost:3000`).
//!
//! Each connection the phone's web view opens becomes one TCP connection to the shown port
//! here, its bytes carried on the Blob channel of the phone's own connection with the phone's
//! number for it as the stream. Every frame is whole (END set), so the reassembler and its
//! deadline never see a tunnel, and an empty frame ends one side. Only the port of a recorded
//! loopback show is dialed: the phone cannot name any other.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use qoop_proto::{Channel, Flags, Frame};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::control::{BLOB_MOST, ControlResponse};
use crate::store::StoreHandle;

/// Frames from the phone waiting for their dev server. Past it the tunnel is closed: the
/// read loop never waits on a socket, or one stuck page would stop the typing.
const QUEUED: usize = 64;

/// The most tunnels one connection holds open.
const MOST_OPEN: usize = 32;

/// How long a dial to the dev server may take.
const DIAL: Duration = Duration::from_secs(3);

/// How long the phone's side may go on after the dev server closed its own.
const GRACE: Duration = Duration::from_secs(2);

/// The writer's lane for tunnel bytes, and how many of its frames go out per pass, after the
/// answers and the screens: a burst of a 5 MB bundle never stands in front of either.
pub(crate) const LANE: usize = 8;
pub(crate) const PER_PASS: usize = 4;

/// Which tunnel a map entry is, so a task removes only its own.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Tunnel {
    to_mac: mpsc::Sender<Bytes>,
    generation: u64,
    /// Dropped with the entry, which is what tells the task to stop.
    _closing: oneshot::Sender<()>,
}

/// One connection's tunnels, by the phone's number for each.
pub(crate) type Tunnels = Arc<Mutex<HashMap<u32, Tunnel>>>;

/// Where a tunnel's answer and its bytes go: the connection's control and blob lanes.
#[derive(Clone)]
pub(crate) struct Lanes {
    pub control: mpsc::Sender<BytesMut>,
    pub blob: mpsc::Sender<BytesMut>,
}

fn frame(conn: u32, bytes: &[u8]) -> BytesMut {
    let mut out = BytesMut::new();
    let _ = Frame::new(Channel::Blob, conn, Bytes::copy_from_slice(bytes)).encode(&mut out);
    out
}

/// `TunnelOpen`, on the read loop: `conn` is taken at once, so a second open of it is
/// refused, and the show lookup and the dial run in a task of their own, which answers.
pub(crate) fn open(
    tunnels: &Tunnels,
    id: String,
    show: u64,
    conn: u32,
    store: StoreHandle,
    lanes: Lanes,
) -> Option<ControlResponse> {
    let mut open = tunnels.lock().unwrap();
    let refused = if open.contains_key(&conn) {
        format!("tunnel {conn} is already open")
    } else if open.len() >= MOST_OPEN {
        format!("at most {MOST_OPEN} tunnels are open at once")
    } else {
        let (to_mac, from_phone) = mpsc::channel(QUEUED);
        let (_closing, closed) = oneshot::channel();
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        let tunnel = Carry {
            tunnels: tunnels.clone(),
            conn,
            generation,
            lanes,
        };
        // Spawned under the lock, so the task cannot end and look for itself before it is in.
        tokio::spawn(tunnel.carry(id, show, store, from_phone, closed));
        let entry = Tunnel {
            to_mac,
            generation,
            _closing,
        };
        open.insert(conn, entry);
        return None;
    };
    Some(ControlResponse::Failed {
        id,
        reason: refused,
    })
}

struct Carry {
    tunnels: Tunnels,
    conn: u32,
    generation: u64,
    lanes: Lanes,
}

async fn dial(show: u64, store: &StoreHandle) -> Result<TcpStream, String> {
    let port = crate::shows::port_of(show, store).await?;
    match tokio::time::timeout(DIAL, TcpStream::connect(("localhost", port))).await {
        Ok(Ok(stream)) => Ok(stream),
        _ => Err(format!("nothing is listening on port {port}")),
    }
}

impl Carry {
    async fn carry(
        self,
        id: String,
        show: u64,
        store: StoreHandle,
        from_phone: mpsc::Receiver<Bytes>,
        mut closed: oneshot::Receiver<()>,
    ) {
        let dialed = tokio::select! {
            dialed = dial(show, &store) => dialed,
            _ = &mut closed => Err("the tunnel was closed before it opened".to_string()),
        };
        let answer = match &dialed {
            Ok(_) => ControlResponse::TunnelOpened {
                id,
                conn: self.conn,
            },
            Err(reason) => ControlResponse::Failed {
                id,
                reason: reason.clone(),
            },
        };
        if let Ok(out) = crate::daemon::control_bytes(&answer) {
            let _ = self.lanes.control.send(out).await;
        }
        if let Ok(stream) = dialed {
            tokio::select! {
                () = self.pump(stream, from_phone) => {}
                // Torn: the phone is told, waiting for room in the lane rather than losing it.
                _ = &mut closed => {
                    let _ = self.lanes.blob.send(frame(self.conn, &[])).await;
                }
            }
        }
        let mut open = self.tunnels.lock().unwrap();
        if open
            .get(&self.conn)
            .is_some_and(|t| t.generation == self.generation)
        {
            open.remove(&self.conn);
        }
    }

    /// Both ways until both sides ended, or one side a little after the other's end.
    async fn pump(&self, stream: TcpStream, mut from_phone: mpsc::Receiver<Bytes>) {
        let (mut rd, mut wr) = stream.into_split();
        let up = async {
            while let Some(bytes) = from_phone.recv().await {
                if bytes.is_empty() {
                    let _ = wr.shutdown().await;
                    return;
                }
                if wr.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        };
        // Awaited into the lane: a slow phone pauses this read, and so the dev server.
        let down = async {
            let mut buf = vec![0; BLOB_MOST];
            loop {
                let n = rd.read(&mut buf).await.unwrap_or(0);
                let sent = self.lanes.blob.send(frame(self.conn, &buf[..n])).await;
                if sent.is_err() || n == 0 {
                    return;
                }
            }
        };
        tokio::pin!(up, down);
        tokio::select! {
            () = &mut up => {
                let _ = tokio::time::timeout(GRACE, down).await;
            }
            () = &mut down => {
                let _ = tokio::time::timeout(GRACE, up).await;
            }
        }
    }
}

/// A Blob frame from the phone, never awaited here. A frame without END or over
/// [`BLOB_MOST`], or one that finds its tunnel's queue full, closes that tunnel only (its task
/// tells the phone); a frame for no open tunnel is dropped.
pub(crate) fn take(tunnels: &Tunnels, from_phone: Frame) {
    let conn = from_phone.stream;
    let mut open = tunnels.lock().unwrap();
    let Some(tunnel) = open.get(&conn) else {
        return;
    };
    let whole = from_phone.flags.contains(Flags::END) && from_phone.body.len() <= BLOB_MOST;
    if whole && tunnel.to_mac.try_send(from_phone.body).is_ok() {
        return;
    }
    open.remove(&conn);
}

/// The connection ended: every tunnel of it ends too.
pub(crate) fn end(tunnels: &Tunnels) {
    tunnels.lock().unwrap().clear();
}
