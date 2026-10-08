//! Scenario 2: attach latency, real process start to the first rendered frame.
//!
//! Section 7's "attach to a small workspace" budget is a client joining an already running
//! daemon, not a cold daemon start: G1.1 already covers cold start, on a 2 second budget two
//! orders of magnitude looser than this one. So one real `qoopd` is spawned once and kept
//! warm, and each sample spawns a fresh real `qoop` client process under its own PTY,
//! timing from that process's spawn to the first bytes of a rendered frame reaching the
//! outer terminal.
//!
//! A rendered frame is recognised by the first cursor-addressing sequence
//! (`CSI params H`): the attach preamble disables mouse modes and shows the
//! cursor, which are `h`/`l` sequences, while every painted frame moves the
//! cursor with `H`. Measured against the real client: its first bytes are
//! `ESC[?1000l...` preamble followed by `ESC[1;32H...` paint, with no bare
//! `ESC[H` anywhere, so matching only the bare home sequence never fires.

use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use qoop_pty::sys;

use crate::proc::DaemonProcess;
use crate::report::{Budget, CheckStat, ScenarioOutcome};
use crate::stats::summarize;

const WARMUP: usize = 3;
const SAMPLES: usize = 30;
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
    let mut child = qoop_testkit::sandboxed(bin)
        .env("QOOP_RUNTIME_DIR", runtime_dir)
        // Same session pinning as the daemon spawn above: the client must
        // attach the session the warm daemon actually holds.
        .env_remove(qoop_runtime::paths::SESSION_ENV)
        // And the same token hygiene: a bench run inside a qoop terminal
        // must not leak that terminal's identity into the client.
        .env_remove("QOOP_TOKEN")
        .env_remove("QORO_TOKEN")
        .env_remove("QOOP_TERMINAL")
        .env_remove("QORO_TERMINAL")
        .stdin(dup_slave(&pty)?)
        .stdout(dup_slave(&pty)?)
        .stderr(Stdio::null())
        .spawn()?;
    // The parent must not keep the slave, or the master never sees end of stream once the
    // client process exits. Same reasoning as `qoop-runtime`'s own PTY spawn.
    drop(pty.slave);

    let result = read_until_first_frame(pty.master.as_raw_fd(), start);

    let _ = child.kill();
    let _ = child.wait();
    result
}

fn dup_slave(pty: &qoop_pty::sys::PtyPair) -> std::io::Result<Stdio> {
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
            if contains_cup(&seen) {
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

/// Whether the bytes so far contain a cursor-addressing sequence: `ESC [`
/// followed by digits and semicolons, terminated by `H`. The bare home
/// `ESC[H` is one instance; the client paints with explicit coordinates
/// (`ESC[1;32H`), which is another. The startup preamble's `h`/`l` sequences
/// never match, so the first match is the first painted frame.
fn contains_cup(haystack: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 <= haystack.len() {
        if haystack[i] == 0x1b && haystack[i + 1] == b'[' {
            let mut j = i + 2;
            while j < haystack.len() && (haystack[j].is_ascii_digit() || haystack[j] == b';') {
                j += 1;
            }
            if j < haystack.len() && haystack[j] == b'H' {
                return true;
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::contains_cup;

    #[test]
    fn the_preamble_alone_is_not_a_frame() {
        assert!(!contains_cup(b"\x1b[?1000l\x1b[?1002l\x1b[?25h"));
    }

    #[test]
    fn a_positioned_cursor_is_a_frame() {
        assert!(contains_cup(b"\x1b[?25h\x1b[1;32Hhello"));
        assert!(contains_cup(b"\x1b[H"));
    }
}
