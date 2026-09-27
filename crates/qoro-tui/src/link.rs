//! One remote daemon, reached through the system `ssh` running `qoro relay` over there.
//!
//! The relay speaks the same protocol as a local socket, so what this returns is a byte
//! stream that has already done its handshake. Everything above it treats the remote as a
//! daemon like any other.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use qoro_remote::SshTransport;
use qoro_remote::saved::{MARKER_LIMIT, RELAY_MARKER, Remote, remote_command};
use qoro_runtime::client;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// The stream to a remote daemon: ssh's stdout to read, its stdin to write.
pub type Upstream = tokio::io::Join<BufReader<ChildStdout>, ChildStdin>;

/// How long a connection may take from spawning ssh to the daemon's welcome. A host that
/// answers nothing in that time is shown as offline and tried again later, rather than
/// holding a space in "connecting" forever.
const OPEN_DEADLINE: Duration = Duration::from_secs(20);

pub struct Link {
    pub stream: Upstream,
    pub leftover: BytesMut,
    /// Held so the connection lives exactly as long as this does: dropping it kills ssh.
    pub child: Child,
    /// The last line ssh wrote to stderr, which is the best account there is of why a
    /// connection ended. Empty when stderr goes to the terminal instead.
    pub said: Arc<Mutex<String>>,
}

/// A private directory for ssh's control sockets, and the path pattern inside it.
///
/// `%C` is a fixed-length hash of the connection, so the path has a known length: a unix
/// socket path is capped near 104 bytes and ssh appends a random suffix while it creates
/// the master. A home deep enough to break that gets no connection sharing rather than a
/// connection that fails.
pub fn control_path() -> Option<String> {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".cache"))
                .ok()
        })?
        .join("qoro");
    let dir = base.to_str()?.to_string();
    // cm-, 40 hex characters, and the 17 byte suffix ssh adds while binding.
    if dir.len() + 1 + 3 + 40 + 17 > 100 {
        return None;
    }
    std::fs::create_dir_all(&base).ok()?;
    std::fs::set_permissions(&base, std::os::unix::fs::PermissionsExt::from_mode(0o700)).ok()?;
    Some(format!("{dir}/cm-%C"))
}

/// Spawn ssh to `remote`, skip whatever its shell printed, and do the handshake.
///
/// `batch` is for a connection nobody is watching: it never prompts, and ssh's complaint
/// is kept for the offline row instead of being written over the interface.
pub async fn open(remote: &Remote, batch: bool) -> io::Result<Link> {
    let mut transport = SshTransport::new(&remote.target);
    if let Some(path) = control_path() {
        transport = transport.with_control_path(path);
    }
    if batch {
        transport = transport.batch();
    }
    let argv = transport.argv(&remote_command(&remote.session, "relay"));
    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(if batch {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::inherit()
        })
        .kill_on_drop(true)
        .spawn()?;
    let said = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let said = said.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if !line.is_empty()
                    && let Ok(mut said) = said.lock()
                {
                    *said = line.to_string();
                }
            }
        });
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("ssh has no stdin"))?;
    let connected = tokio::time::timeout(OPEN_DEADLINE, async {
        let mut out = BufReader::new(stdout);
        skip_to_marker(&mut out).await?;
        client::handshake(tokio::io::join(out, stdin), "qoro-tui").await
    })
    .await;
    let failure = |e: io::Error| {
        // Give ssh's own words a moment to arrive: they are the reason, and the error from
        // the pipe is only that the pipe closed.
        let said = said.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let said = said.lock().map(|s| s.clone()).unwrap_or_default();
            if said.is_empty() {
                e
            } else {
                io::Error::new(e.kind(), said)
            }
        }
    };
    match connected {
        Ok(Ok((stream, leftover))) => Ok(Link {
            stream,
            leftover,
            child,
            said,
        }),
        Ok(Err(e)) => Err(failure(e).await),
        Err(_) => Err(failure(io::Error::new(
            io::ErrorKind::TimedOut,
            "the remote did not answer in time",
        ))
        .await),
    }
}

/// Read up to and including the marker line, keeping every byte after it buffered.
///
/// A line is compared whole after trimming, so a banner that happens to contain the marker
/// text inside a longer line is still a banner.
async fn skip_to_marker<R>(out: &mut BufReader<R>) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut seen = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut *out)
            .take((MARKER_LIMIT - seen) as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the remote closed the connection before qoro started",
            ));
        }
        seen += n;
        if line.trim_ascii() == RELAY_MARKER.as_bytes() {
            return Ok(());
        }
        if seen > MARKER_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the remote shell printed too much before qoro started",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn skip(input: &'static [u8]) -> (io::Result<()>, Vec<u8>) {
        let mut out = BufReader::new(input);
        let result = skip_to_marker(&mut out).await;
        let mut rest = Vec::new();
        let _ = out.read_to_end(&mut rest).await;
        (result, rest)
    }

    /// A login banner, then the marker, then binary protocol bytes: the banner goes, and
    /// every byte after the marker survives, including the ones already buffered.
    #[tokio::test]
    async fn a_banner_is_skipped_and_the_protocol_after_it_is_kept() {
        let (result, rest) =
            skip(b"Welcome to devbox\nlast login: never\n-----qoro-relay-----\n\x00\x01binary")
                .await;
        result.expect("marker found");
        assert_eq!(rest, b"\x00\x01binary");
    }

    #[tokio::test]
    async fn a_marker_inside_a_longer_line_is_not_the_marker() {
        let (result, _) = skip(b"echo -----qoro-relay----- later\n").await;
        assert_eq!(
            result.expect_err("eof").kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn a_shell_that_never_starts_qoro_is_cut_off() {
        let flood: &'static [u8] = Box::leak(vec![b'x'; MARKER_LIMIT * 2].into_boxed_slice());
        let (result, _) = skip(flood).await;
        assert_eq!(
            result.expect_err("too much").kind(),
            io::ErrorKind::InvalidData
        );
    }
}
