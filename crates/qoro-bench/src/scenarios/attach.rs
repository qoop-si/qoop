//! Scenario 2: attach latency, real process start to the first rendered frame.
//!
//! Section 7's "attach to a small workspace" budget is a client joining an already running
//! daemon, not a cold daemon start: G1.1 already covers cold start, on a 2 second budget two
//! orders of magnitude looser than this one. So one real `qorod` is spawned once and kept
//! warm, and each sample spawns a fresh real `qoro` client process under its own PTY,
//! timing from that process's spawn to the first bytes of a rendered frame reaching the
//! outer terminal.
//!
//! `qoro_runtime::client::render` always begins a frame with `ESC [ H` (home cursor), and
//! nothing else the client writes on startup contains that sequence: the attach preamble
//! disables mouse modes and shows the cursor, neither of which is `\x1b[H`. That makes the
//! byte sequence a real signal of "a snapshot was drawn", not a guess about timing.

use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use qoro_pty::sys;

use crate::proc::DaemonProcess;
use crate::report::{Budget, CheckStat, ScenarioOutcome};
use crate::stats::summarize;

const WARMUP: usize = 3;
const SAMPLES: usize = 30;
const FIRST_FRAME_MARKER: &[u8] = b"\x1b[H";
const PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(bin: &Path) -> ScenarioOutcome {
    let name = "attach to a running daemon".to_string();
    if !bin.is_file() {
        return ScenarioOutcome::failed(name, format!("binary not found at {}", bin.display()));
    }

    let daemon = match DaemonProcess::spawn(bin) {
        Ok(d) => d,
        Err(e) => {
            return ScenarioOutcome::failed(name, format!("could not start a warm daemon: {e}"));
        }
    };

    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..(WARMUP + SAMPLES) {
        match attach_once(bin, daemon.dir()) {
            Ok(elapsed) => {
                if i >= WARMUP {
                    samples.push(elapsed.as_secs_f64() * 1000.0);
                }
            }
            Err(e) => return ScenarioOutcome::failed(name, format!("sample {i}: {e}")),
        }
    }

    let budget = Budget {
        stat: CheckStat::P95,
        limit: 75.0,
        description: "p95 under 75 ms, section 7",
    };
    ScenarioOutcome::measured(name, "ms", summarize(&samples), Some(budget))
}

/// Spawn one real client process attached to `runtime_dir` under a fresh PTY, and time until
/// its first rendered frame appears.
fn attach_once(bin: &Path, runtime_dir: &Path) -> std::io::Result<Duration> {
    let pty = sys::open_pty(80, 24)?;

    let start = Instant::now();
    let mut child = Command::new(bin)
        .env("QORO_RUNTIME_DIR", runtime_dir)
        .stdin(dup_slave(&pty)?)
        .stdout(dup_slave(&pty)?)
        .stderr(Stdio::null())
        .spawn()?;
    // The parent must not keep the slave, or the master never sees end of stream once the
    // client process exits. Same reasoning as `qoro-runtime`'s own PTY spawn.
    drop(pty.slave);

    let result = read_until_first_frame(pty.master.as_raw_fd(), start);

    let _ = child.kill();
    let _ = child.wait();
    result
}

fn dup_slave(pty: &qoro_pty::sys::PtyPair) -> std::io::Result<Stdio> {
    // SAFETY: the slave fd is live for the duration of this call; `dup` returns a fresh
    // descriptor owned by nobody else, which is wrapped immediately.
    let d = unsafe { libc::dup(pty.slave.as_raw_fd()) };
    if d < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `d` is fresh and has no other owner.
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(d) };
    Ok(Stdio::from(owned))
}

fn read_until_first_frame(
    master_fd: std::os::fd::RawFd,
    start: Instant,
) -> std::io::Result<Duration> {
    let deadline = start + PER_ATTEMPT_TIMEOUT;
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "no rendered frame within 5s",
            ));
        }
        // SAFETY: `master_fd` is a live, non-blocking pty master owned by the caller for the
        // duration of this loop, and `buf` is a valid buffer of the given length.
        let n = unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
            if contains(&seen, FIRST_FRAME_MARKER) {
                return Ok(Instant::now().duration_since(start));
            }
        } else if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the client exited before rendering anything",
            ));
        } else {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(Duration::from_millis(1));
            } else {
                return Err(err);
            }
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
