//! qoop run as `arecord`: the microphone of the qoop window a person types into, lent to a
//! program in a tab on a machine that has none (`qoop_runtime::microphone`).
//!
//! Claude Code's voice, on a machine with no sound card, checks `arecord --version`, probes
//! `arecord -f S16_LE -r 16000 -c 1 -t raw /dev/null` and records with the same arguments
//! and `-q -`, reading 16 kHz mono samples from standard output until it sends SIGTERM. The
//! daemon puts this binary first on its tabs' PATH under that name (Linux only). Exactly
//! that shape is answered here; anything else is handed to the next `arecord` on PATH, so
//! a person's own recording on a machine with a real one is never swallowed.

use std::ffi::OsString;
use std::io::Write as _;
use std::time::Duration;

use qoop_runtime::client;
use qoop_runtime::control::{ControlRequest, ControlResponse};

/// How long a recording may hear nothing before it ends: no window lent its microphone, or
/// the one that did has gone.
const SILENCE: Duration = Duration::from_secs(3);

/// How long a refused recording is asked again: the last one's end may not have reached
/// the daemon yet when Claude Code starts the next.
const RETRY: Duration = Duration::from_millis(300);

/// Whether this process was started as `arecord`.
pub fn called() -> bool {
    std::env::args_os()
        .next()
        .map(std::path::PathBuf::from)
        .and_then(|p| p.file_name().map(|n| n == "arecord"))
        .unwrap_or(false)
}

#[derive(Debug, PartialEq)]
enum Asked {
    Version,
    Probe,
    Record,
}

/// What Claude Code asks, or `None` for anything else.
fn asked(args: &[OsString]) -> Option<Asked> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    if args == ["--version"] {
        return Some(Asked::Version);
    }
    let mut wanted = vec![
        ("-f", "S16_LE"),
        ("-r", "16000"),
        ("-c", "1"),
        ("-t", "raw"),
    ];
    let mut out = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg {
            "-q" => {}
            "-" | "/dev/null" if out.is_none() => out = Some(arg),
            flag => {
                let value = it.next()?;
                let at = wanted.iter().position(|w| *w == (flag, value))?;
                wanted.remove(at);
            }
        }
    }
    if !wanted.is_empty() {
        return None;
    }
    match out? {
        "-" => Some(Asked::Record),
        _ => Some(Asked::Probe),
    }
}

/// Run as `arecord`, and say how to exit.
pub fn run() -> i32 {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let terminal = qoop_runtime::legacy::var("QOOP_TERMINAL")
        .ok()
        .and_then(|t| t.parse::<u32>().ok());
    let (asked, terminal) = match (asked(&args), terminal) {
        (Some(Asked::Version), _) => {
            println!("arecord: qoop, lending the microphone of the qoop window you type into");
            return 0;
        }
        (Some(asked), Some(terminal)) => (asked, terminal),
        _ => return hand_on(&args),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return said(&e.to_string()),
    };
    let done = runtime.block_on(async {
        match asked {
            Asked::Probe => probe(terminal).await,
            _ => record(terminal).await,
        }
    });
    match done {
        Ok(()) => 0,
        Err(e) => said(&e.to_string()),
    }
}

fn said(why: &str) -> i32 {
    eprintln!("arecord: {why}");
    1
}

/// The next `arecord` on PATH that is not this binary, run in this process's place.
/// Set on what this hands on to: a second qoop `arecord` further along PATH (another
/// install, an isolated daemon's) hands on no further, so two never exec each other forever.
const HANDED: &str = "QOOP_ARECORD_HANDED";

fn hand_on(args: &[OsString]) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let none = "no microphone here, and no qoop window lends one";
    let Ok(me) = std::env::current_exe().and_then(|p| p.canonicalize()) else {
        return said(none);
    };
    if std::env::var_os(HANDED).is_some() {
        return said(none);
    }
    let next = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join("arecord"))
        .find(|p| p.is_file() && p.canonicalize().ok().is_some_and(|p| p != me));
    match next {
        Some(next) => said(
            &std::process::Command::new(next)
                .args(args)
                .env(HANDED, "1")
                .exec()
                .to_string(),
        ),
        None => said(none),
    }
}

/// A connection to this machine's daemon that only watches.
async fn connect() -> std::io::Result<(tokio::net::UnixStream, bytes::BytesMut)> {
    let dir = qoop_runtime::paths::runtime_dir().map_err(std::io::Error::other)?;
    client::connect_observer(&qoop_runtime::paths::socket_path(&dir), "arecord").await
}

async fn probe(terminal: u32) -> std::io::Result<()> {
    let (stream, inbuf) = connect().await?;
    let request = ControlRequest::Listen {
        id: crate::request_id("arecord"),
        terminal,
        probe: true,
        token: token(),
    };
    match crate::ask_over(stream, inbuf, &request, Duration::from_secs(2)).await? {
        ControlResponse::Listening { .. } => Ok(()),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

/// Write what the lent microphone hears to standard output until this process is ended,
/// standard output is closed, or nothing is heard for `SILENCE`.
async fn record(terminal: u32) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (stream, mut inbuf) = connect().await?;
    let (mut rd, mut wr) = stream.into_split();
    let mut seq = 0u32;
    let mut connection = client::Connection::new();
    let mut read = [0u8; 64 * 1024];
    // Screens of the tab this connection was given by default would only be read and
    // thrown away: none are wanted.
    let mut ask = |request: ControlRequest| client::encode_control_seq(&request, &mut seq);
    wr.write_all(&ask(ControlRequest::ListTerminals {
        id: "arecord-list".into(),
    })?)
    .await?;
    let first = tokio::time::Instant::now();
    let mut id = crate::request_id("arecord");
    wr.write_all(&ask(listen(&id, terminal))?).await?;
    let mut stdout = std::io::stdout().lock();
    let mut heard_at = tokio::time::Instant::now();
    loop {
        let quiet = heard_at + SILENCE;
        let n = match tokio::time::timeout_at(quiet, rd.read(&mut read)).await {
            Err(_) => return Err(std::io::Error::other("the qoop window lent no microphone")),
            Ok(n) => n?,
        };
        if n == 0 {
            return Err(std::io::Error::other("the qoop daemon went away"));
        }
        inbuf.extend_from_slice(&read[..n]);
        for incoming in connection.drain(&mut inbuf)? {
            let client::Incoming::Control(response) = incoming else {
                continue;
            };
            match response {
                ControlResponse::Terminals { terminals, .. } => {
                    let request = ControlRequest::Unsubscribe {
                        id: "arecord-quiet".into(),
                        terminals,
                    };
                    wr.write_all(&ask(request)?).await?;
                }
                ControlResponse::Heard { id: of, data } if of == id => {
                    heard_at = tokio::time::Instant::now();
                    let Some(bytes) = qoop_runtime::microphone::decode(&data) else {
                        continue;
                    };
                    // A reader that stopped reading is a recording that is over.
                    if stdout
                        .write_all(&bytes)
                        .and_then(|()| stdout.flush())
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                ControlResponse::Failed { id: of, reason } if of == id => {
                    if first.elapsed() > RETRY {
                        return Err(std::io::Error::other(reason));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    id = crate::request_id("arecord");
                    wr.write_all(&ask(listen(&id, terminal))?).await?;
                }
                _ => {}
            }
        }
    }
}

fn listen(id: &str, terminal: u32) -> ControlRequest {
    ControlRequest::Listen {
        id: id.to_string(),
        terminal,
        probe: false,
        token: token(),
    }
}

/// This tab's token, which only a program in it has: the daemon lends to nobody else.
fn token() -> String {
    qoop_runtime::legacy::var("QOOP_TOKEN").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<OsString> {
        line.split_whitespace().map(OsString::from).collect()
    }

    #[test]
    fn only_claude_codes_shape_is_taken() {
        assert_eq!(asked(&args("--version")), Some(Asked::Version));
        assert_eq!(
            asked(&args("-f S16_LE -r 16000 -c 1 -t raw /dev/null")),
            Some(Asked::Probe)
        );
        assert_eq!(
            asked(&args("-f S16_LE -r 16000 -c 1 -t raw -q -")),
            Some(Asked::Record)
        );
        // Anything else is a person's own recording, handed on.
        for line in [
            "take.wav",
            "-f cd take.wav",
            "-f S16_LE -r 44100 -c 1 -t raw -",
            "-f S16_LE -r 16000 -c 1 -t raw out.raw",
            "-f S16_LE -r 16000 -c 1 -",
            "-f S16_LE -r 16000 -c 1 -t raw -d 5 -",
            "-l",
            "",
        ] {
            assert_eq!(asked(&args(line)), None, "{line}");
        }
    }
}
