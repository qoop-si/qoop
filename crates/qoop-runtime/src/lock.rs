//! Daemon singleton election.
//!
//! A readiness pipe and an inode check do not serialise two simultaneous cold starts: a
//! second launcher can see a bound-but-not-yet-listening socket, classify it as stale,
//! unlink it and bind its own, leaving two daemons and one unreachable workspace. An
//! exclusive `flock` held across the whole sequence removes the window, and it is released
//! automatically when the process exits, so a crash mid-sequence leaves nothing to clean up.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// An exclusive lock over the startup sequence: stale-socket check, unlink, bind, listen,
/// initialise, signal readiness.
pub struct StartupLock {
    _file: File,
}

impl StartupLock {
    /// Block until the lock is held.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: flock takes a live descriptor and a flag, and cannot violate any Rust
        // invariant. The lock is tied to the open file description, so it is released when
        // the file is dropped or the process exits.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { _file: file })
    }

    /// The lock a daemon that moved onto this binary was holding, from the descriptor it
    /// passed on. `flock` belongs to the open file, so the lock is held the whole way across
    /// the `exec`: nobody could take it between the two daemons.
    ///
    /// # Safety
    /// `fd` must be the descriptor of a file this process was handed for this lock and
    /// nothing else owns.
    // SAFETY: the contract is the `# Safety` section above; each caller says how it meets it.
    pub unsafe fn from_fd(fd: std::os::fd::RawFd) -> Self {
        use std::os::fd::FromRawFd;
        // SAFETY: the caller says the descriptor is ours to own.
        Self {
            _file: unsafe { File::from_raw_fd(fd) },
        }
    }

    /// The descriptor the lock is held on, for passing across an `exec`.
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self._file.as_raw_fd()
    }

    /// Take the lock only if it is free. Used by tests to observe contention.
    pub fn try_acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        // SAFETY: as above.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_exclusive_while_held_and_free_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qoopd.lock");
        let held = StartupLock::acquire(&path).unwrap();
        assert!(
            StartupLock::try_acquire(&path).unwrap().is_none(),
            "a second acquire must not succeed while the first is held"
        );
        drop(held);

        // Retried, and the reason is a fork in another thread rather than a lock that did
        // not release. `flock` belongs to the open file description, a `fork` copies it, and
        // the copy holds the lock until the child's `exec` closes it: this crate's own tests
        // spawn processes, so a fork landing between the acquire above and the drop here
        // keeps the file locked for as long as that child takes to exec. Observed once in a
        // full parallel workspace run and never in twelve runs of this binary alone, which
        // is the shape of a window that only opens on a loaded machine.
        //
        // Still an assertion, not a sleep: a lock that is genuinely stuck stays stuck for
        // two seconds and fails.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut freed = StartupLock::try_acquire(&path).unwrap().is_some();
        while !freed && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            freed = StartupLock::try_acquire(&path).unwrap().is_some();
        }
        assert!(freed, "the lock must be free once released");
    }

    #[test]
    fn a_lock_passed_on_as_a_descriptor_stays_held_by_whoever_takes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qoopd.lock");
        let held = StartupLock::acquire(&path).unwrap();
        // What an `exec` does to a descriptor that is not close-on-exec: the same open file,
        // at a number of its own, in whatever becomes of the process.
        // SAFETY: dup of a live descriptor, owned by nothing else once taken.
        let passed = unsafe { libc::dup(held.as_raw_fd()) };
        assert!(passed >= 0);
        drop(held);
        // SAFETY: `passed` was just made for this lock and nothing else owns it.
        let taken = unsafe { StartupLock::from_fd(passed) };
        assert!(
            StartupLock::try_acquire(&path).unwrap().is_none(),
            "the lock was not held across the hand over"
        );
        drop(taken);
        assert!(StartupLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn a_crashed_holder_releases_the_lock() {
        // The lock lives on the open file description, so there is no stale lock to clean
        // up after a launcher that died mid-sequence. Modelled with a real child process.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qoopd.lock");
        let p = path.to_string_lossy().to_string();
        let mut child = std::process::Command::new("/usr/bin/env")
            .args([
                "python3",
                "-c",
                &format!(
                    "import fcntl,os,time; f=open({p:?},'w'); fcntl.flock(f, fcntl.LOCK_EX); \
                     print('locked', flush=True); time.sleep(30)"
                ),
            ])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn a lock holder");

        // Wait until it reports that it holds the lock.
        use std::io::{BufRead, BufReader};
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        assert!(line.contains("locked"));
        assert!(StartupLock::try_acquire(&path).unwrap().is_none());

        child.kill().unwrap();
        child.wait().unwrap();
        // No unlock ran, and yet the lock is free.
        assert!(
            StartupLock::try_acquire(&path).unwrap().is_some(),
            "a killed holder must not leave a stale lock"
        );
    }
}
