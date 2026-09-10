//! Noticing when a terminal's foreground process is replaced.
//!
//! ADR-009. Two mechanisms, because the platforms genuinely differ and pretending otherwise
//! would mean polling on the one that does not need it.
//!
//! On macOS, `kqueue` with `EVFILT_PROC` reports `NOTE_EXEC` and `NOTE_EXIT` for a pid we
//! do not own, without privileges, and the kqueue descriptor is itself pollable. That is a
//! subscription: nothing runs until something happens, which is the rule the whole runtime
//! is built on.
//!
//! On Linux there is no unprivileged equivalent. `pidfd` reports exit and nothing else, and
//! an in place `exec` changes `/proc/<pid>/exe` with no notification at all. So Linux gets
//! an adaptive probe: fast while something is changing, backing off to a floor while
//! nothing is, and the schedule is a value a gate can assert against rather than a constant
//! buried in a loop.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

use crate::teardown::Pid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcEvent {
    /// The process replaced its program image. Same pid, different program: the case that
    /// makes a pid a bad identity for an agent.
    Exec(Pid),
    /// The process forked. Not interesting in itself, and the only warning we get that the
    /// foreground process group is about to change: a shell running a command forks, then
    /// moves the child into the foreground, and a change with no output would otherwise be
    /// invisible until the child printed something.
    Fork(Pid),
    Exit(Pid),
}

/// The probe schedule for the platform that has to poll.
///
/// Exposed so a gate can assert the schedule rather than the timing, which is the
/// difference between a test that means something on a loaded runner and one that does not.
pub const PROBE_FAST: Duration = Duration::from_millis(50);
pub const PROBE_FLOOR: Duration = Duration::from_secs(60);
/// How many quiet probes before the interval doubles.
pub const PROBE_PATIENCE: u32 = 4;

/// The next probe interval, given the current one and whether anything changed.
///
/// A pure function so the schedule is testable without waiting for it.
pub fn next_probe(current: Duration, changed: bool) -> Duration {
    if changed {
        return PROBE_FAST;
    }
    let doubled = current.saturating_mul(2);
    if doubled > PROBE_FLOOR {
        PROBE_FLOOR
    } else {
        doubled
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub struct ProcWatch {
        kq: OwnedFd,
    }

    impl ProcWatch {
        pub fn new() -> io::Result<Self> {
            // SAFETY: kqueue takes no arguments and returns a descriptor or -1.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            crate::sys::set_cloexec(fd)?;
            // SAFETY: fd is fresh and unowned.
            Ok(Self {
                kq: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }

        pub fn raw_fd(&self) -> RawFd {
            self.kq.as_raw_fd()
        }

        /// Subscribe to a pid.
        ///
        /// Registering a pid that has already exited returns `ESRCH`, which is not an error
        /// the caller can do anything about and is reported as `Ok(false)`: the process is
        /// gone, which is exactly what the caller wanted to hear about.
        pub fn watch(&self, pid: Pid) -> io::Result<bool> {
            let ev = libc::kevent {
                ident: pid.0 as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_EXEC | libc::NOTE_EXIT | libc::NOTE_FORK,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: kq is live, ev is one valid kevent, and no output is requested.
            let rc = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    &ev,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(false);
                }
                return Err(err);
            }
            Ok(true)
        }

        pub fn unwatch(&self, pid: Pid) {
            let ev = libc::kevent {
                ident: pid.0 as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_DELETE,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: as above; a failure here means it was already gone.
            unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    &ev,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
        }

        /// Everything that has happened, without blocking.
        ///
        /// The buffer holds one event per watched process and then some. Thirty two was the
        /// first size and it is the kind of number that looks generous and is not: a
        /// hundred terminals starting a command at the same moment produce more than that
        /// in one go, the rest are left in the queue, and the descriptor's readiness edge
        /// has already been consumed, so nothing comes back for them. Measured: 14 of 100
        /// terminals noticed their own foreground process, against 100 of 100 with a
        /// buffer that fits the burst.
        ///
        /// Sixteen kilobytes of stack, once per drain, which is the cheapest part of this.
        pub fn drain(&self) -> Vec<ProcEvent> {
            let mut events = [libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            }; 512];
            let zero = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: kq is live, events is a valid array of its stated length, and the
            // timeout is a valid zero timespec.
            let n = unsafe {
                libc::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    &zero,
                )
            };
            if n <= 0 {
                return Vec::new();
            }
            let mut out = Vec::new();
            for ev in events.iter().take(n as usize) {
                let pid = Pid(ev.ident as i32);
                // Exit and exec can arrive together when both happened since the last
                // drain. Exit is reported last, because a caller that stops watching on
                // exit would otherwise drop the exec that preceded it.
                if ev.fflags & libc::NOTE_FORK != 0 {
                    out.push(ProcEvent::Fork(pid));
                }
                if ev.fflags & libc::NOTE_EXEC != 0 {
                    out.push(ProcEvent::Exec(pid));
                }
                if ev.fflags & libc::NOTE_EXIT != 0 {
                    out.push(ProcEvent::Exit(pid));
                }
            }
            out
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// The probe. No subscription exists for `exec` on Linux without privileges, so this
    /// records what each watched process was running and reports a change.
    pub struct ProcWatch {
        watched: Mutex<HashMap<i32, String>>,
    }

    fn program(pid: i32) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.display().to_string())
            .or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok())
    }

    impl ProcWatch {
        pub fn new() -> io::Result<Self> {
            Ok(Self {
                watched: Mutex::new(HashMap::new()),
            })
        }

        /// No descriptor to poll: the caller drives this one on the probe schedule.
        pub fn raw_fd(&self) -> RawFd {
            -1
        }

        pub fn watch(&self, pid: Pid) -> io::Result<bool> {
            match program(pid.0) {
                Some(p) => {
                    self.watched.lock().unwrap().insert(pid.0, p);
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        pub fn unwatch(&self, pid: Pid) {
            self.watched.lock().unwrap().remove(&pid.0);
        }

        pub fn drain(&self) -> Vec<ProcEvent> {
            let mut out = Vec::new();
            let mut watched = self.watched.lock().unwrap();
            watched.retain(|pid, was| match program(*pid) {
                None => {
                    out.push(ProcEvent::Exit(Pid(*pid)));
                    false
                }
                Some(now) if now != *was => {
                    out.push(ProcEvent::Exec(Pid(*pid)));
                    *was = now;
                    true
                }
                Some(_) => true,
            });
            out
        }
    }
}

pub use imp::ProcWatch;

/// Whether this platform reports a process change without being asked.
///
/// The caller runs a probe loop only where this is false, which is what keeps the idle path
/// free of syscalls on the platform that does not need them.
pub const fn is_subscription() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_schedule_backs_off_to_a_floor_and_snaps_back() {
        let mut interval = PROBE_FAST;
        for _ in 0..40 {
            interval = next_probe(interval, false);
        }
        assert_eq!(interval, PROBE_FLOOR, "the probe never reached its floor");
        assert_eq!(
            next_probe(interval, true),
            PROBE_FAST,
            "a change must bring the probe back immediately"
        );
    }

    #[test]
    fn an_exec_is_reported_for_a_process_that_keeps_its_pid() {
        // A shell that replaces itself. The pid does not change, which is the whole point:
        // a watcher keyed on the pid alone would see nothing happen.
        let watch = ProcWatch::new().expect("watch");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 0.4; exec /bin/echo replaced"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = Pid(child.id() as i32);
        assert!(
            watch.watch(pid).expect("watch"),
            "the child was already gone"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_exec = false;
        while std::time::Instant::now() < deadline && !saw_exec {
            for event in watch.drain() {
                if event == ProcEvent::Exec(pid) {
                    saw_exec = true;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.wait();
        assert!(saw_exec, "an in place exec was not reported");
    }

    #[test]
    fn watching_a_process_that_is_already_gone_is_not_an_error() {
        let watch = ProcWatch::new().expect("watch");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn");
        let pid = Pid(child.id() as i32);
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !watch.watch(pid).expect("no error"),
            "a reaped pid must be reported as gone, not as watched"
        );
    }
}
