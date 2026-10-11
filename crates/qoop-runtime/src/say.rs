//! What the daemon says, on stderr, without ever failing for it.
//!
//! A daemon started on demand had `/dev/null` for stderr, so what it said went nowhere. It now
//! writes to its log ([`to_file`]), and a write to a file fails where one to `/dev/null` never
//! did: on a full disk. `eprintln!` panics when its write fails, and a panic in the daemon took
//! every tab with it. So the runtime says everything through [`say!`](crate::say), which drops a
//! line it cannot write, and keeps the log it writes to small while it runs. `eprintln!` is
//! refused in this crate (`clippy.toml`).

use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// A log is moved aside to `<name>.old` when it grows past this.
pub const LIMIT: u64 = 100 * 1024;

/// The log stderr points at, once [`to_file`] pointed it there.
static LOG: OnceLock<PathBuf> = OnceLock::new();

/// Whether this daemon serves its socket ([`serving`]): only then is its log moved aside, so one
/// that lost the race to start never moves it under the one serving.
static SERVING: AtomicBool = AtomicBool::new(false);

/// One line, or one rotation, at a time: every runtime thread says things.
static WRITING: Mutex<()> = Mutex::new(());

/// A line on stderr, as `eprintln!` writes it, but dropped when it cannot be written.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::say::line(::std::format_args!($($arg)*))
    };
}

#[doc(hidden)]
pub fn line(args: std::fmt::Arguments<'_>) {
    // One line at a time, so a rotation never lands between the writes of one line. A
    // poisoned lock still holds its guard.
    let _one = WRITING.lock();
    if let Some(path) = LOG.get().filter(|_| SERVING.load(Ordering::Relaxed)) {
        keep_small(path);
    }
    let _ = writeln!(std::io::stderr(), "{args}");
}

/// `log` opened for appending, readable by the person only (it can hold a panic's text), after
/// it was moved aside if it has grown past `limit`.
pub fn open_log(log: &Path, limit: u64) -> Option<std::fs::File> {
    std::fs::create_dir_all(log.parent()?).ok()?;
    if std::fs::metadata(log).is_ok_and(|m| m.len() > limit) {
        let _ = std::fs::rename(log, log.with_extension("log.old"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
        .ok()
}

/// Point stderr at the log `path` when it goes nowhere (`/dev/null`, as every on-demand start
/// gives it) or already is that log (a daemon that moved onto a new binary keeps its stderr).
/// A daemon run in a terminal, or whose stderr a test took, keeps its own. Whether it did.
///
/// Early, before the daemon knows it will serve, so what it says while it takes over the tabs
/// of the one before it is kept; the log is only appended to until [`serving`].
pub fn to_file(path: PathBuf) -> bool {
    let Ok(now) = std::fs::metadata("/dev/fd/2") else {
        return false;
    };
    let nowhere = std::fs::metadata("/dev/null").is_ok_and(|m| same(&m, &now));
    if !nowhere && !std::fs::metadata(&path).is_ok_and(|m| same(&m, &now)) {
        return false;
    }
    let Some(file) = open_log(&path, u64::MAX) else {
        return false;
    };
    if !onto_stderr(&file) {
        return false;
    }
    let _ = LOG.set(path);
    true
}

/// This daemon serves its socket: from now on its log is kept small, starting now.
pub fn serving() {
    let _one = WRITING.lock();
    SERVING.store(true, Ordering::Relaxed);
    if let Some(path) = LOG.get() {
        keep_small(path);
    }
}

/// Past [`LIMIT`], the log is moved aside and a new one takes its place on stderr, so a daemon
/// that runs for weeks keeps two small files; a log that was removed is made again. Another
/// file put at its name is left as it is.
fn keep_small(path: &Path) {
    let Ok(now) = std::fs::metadata("/dev/fd/2") else {
        return;
    };
    match std::fs::metadata(path) {
        Ok(m) if !same(&m, &now) => return,
        Ok(_) if now.len() <= LIMIT => return,
        _ => {}
    }
    if let Some(file) = open_log(path, LIMIT) {
        onto_stderr(&file);
    }
}

fn onto_stderr(file: &std::fs::File) -> bool {
    // SAFETY: `file` is open for the length of the call and 2 is a valid target; dup2 only
    // replaces what fd 2 refers to, and `file`'s own descriptor closes when it drops.
    unsafe { libc::dup2(file.as_raw_fd(), 2) >= 0 }
}

fn same(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(test)]
mod tests {
    use super::open_log;
    use std::os::unix::fs::PermissionsExt;

    /// Run in a child whose stderr cannot be written (a pipe nobody reads): `eprintln!` panics
    /// there, as it does on a full disk.
    #[test]
    fn a_line_that_cannot_be_written_is_dropped() {
        if std::env::var_os("QOOP_SAY_CHILD").is_some() {
            for n in 0..100 {
                crate::say!("line {n}");
            }
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "say::tests::a_line_that_cannot_be_written_is_dropped",
            ])
            .args(["--nocapture", "--test-threads=1"])
            .env("QOOP_SAY_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stderr.take());
        assert!(
            child.wait().unwrap().success(),
            "a failed write ended the process"
        );
    }

    #[test]
    fn a_log_is_private_and_moved_aside_past_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs/x.log");
        drop(open_log(&log, 10).unwrap());
        let mode = std::fs::metadata(&log).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "readable by others");
        std::fs::write(&log, "more than ten bytes").unwrap();
        drop(open_log(&log, 10).unwrap());
        assert_eq!(
            std::fs::read_to_string(log.with_extension("log.old")).unwrap(),
            "more than ten bytes"
        );
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0, "a fresh log");
    }
}
