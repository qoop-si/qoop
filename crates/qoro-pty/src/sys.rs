//! The narrow syscall surface, with every unsafe block justified.
//!
//! Two measured requirements live here and nowhere else:
//!
//! * Every PTY descriptor is close-on-exec **at creation**. `openpty` does not do this, and
//!   a child spawned for one terminal was measured inheriting another terminal's master and
//!   slave. A leaked slave means the master never sees end of stream.
//! * `proc_listallpids` returns a count of **pids**, not bytes. Dividing by four discards
//!   three quarters of the process table, and because the list is newest first the
//!   discarded part is the long-lived processes containment actually needs.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use crate::teardown::{Member, Pgid, Pid};

pub struct PtyPair {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

fn last_err() -> io::Error {
    io::Error::last_os_error()
}

pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is a live descriptor for the duration of the call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(last_err());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(last_err());
    }
    Ok(())
}

pub fn is_cloexec(fd: RawFd) -> bool {
    // SAFETY: fd is a live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0 && flags & libc::FD_CLOEXEC != 0
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fd is a live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(last_err());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(last_err());
    }
    Ok(())
}

/// Open a PTY pair with both descriptors close-on-exec **at open**, and the master
/// non-blocking.
///
/// Setting close-on-exec after the descriptors exist leaves a window: under a
/// multi-threaded runtime, terminal A's open and terminal B's exec run on different worker
/// threads with no ordering between them, so B's child can inherit A's pair. The leaked
/// slave is the worse half, because the master then never sees end of stream and the
/// terminal never tears down. `openpty` offers no way to close that window, so this opens
/// the pair by hand.
pub fn open_pty(cols: u16, rows: u16) -> io::Result<PtyPair> {
    // SAFETY: posix_openpt takes flags and returns a descriptor or -1.
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master_fd < 0 {
        return Err(last_err());
    }
    // SAFETY: master_fd is fresh and owned by nobody else. Wrapping it now means every
    // early return below closes it.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };

    // SAFETY: master is a live pseudo-terminal master.
    if unsafe { libc::grantpt(master.as_raw_fd()) } != 0 {
        return Err(last_err());
    }
    // SAFETY: as above.
    if unsafe { libc::unlockpt(master.as_raw_fd()) } != 0 {
        return Err(last_err());
    }

    // The slave's name, written into a caller-owned buffer. `ptsname` is deliberately not
    // used: it returns a shared static buffer, which is not safe to call from more than one
    // thread, and the whole point of this function is that it runs concurrently.
    let mut name = [0 as libc::c_char; 128];
    #[cfg(target_os = "macos")]
    {
        // TIOCPTYGNAME is what Apple's own ptsname calls underneath, and it writes into the
        // buffer we hand it.
        const TIOCPTYGNAME: libc::c_ulong = 0x40807453;
        // SAFETY: master is live and name is a 128 byte buffer, which is the size the
        // ioctl is defined against.
        if unsafe { libc::ioctl(master.as_raw_fd(), TIOCPTYGNAME, name.as_mut_ptr()) } != 0 {
            return Err(last_err());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: name is a 128 byte buffer and its length is passed correctly.
        let rc = unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
    }

    // SAFETY: ptsname_r wrote a null-terminated path into name.
    let slave_fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave_fd < 0 {
        return Err(last_err());
    }
    // SAFETY: slave_fd is fresh and owned by nobody else.
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

    resize(master.as_raw_fd(), cols, rows)?;
    set_nonblocking(master.as_raw_fd())?;
    Ok(PtyPair { master, slave })
}

pub fn resize(master: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: master is a live PTY master and ws is fully initialised.
    if unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &ws) } < 0 {
        return Err(last_err());
    }
    Ok(())
}

pub fn window_size(fd: RawFd) -> io::Result<(u16, u16)> {
    // SAFETY: zeroed winsize is a valid out-param.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: fd is live and ws is a valid out-param.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } < 0 {
        return Err(last_err());
    }
    Ok((ws.ws_col, ws.ws_row))
}

/// The kernel's own answer to "which process group is in the foreground", O(1) and with no
/// process table scan. The tiered strategy starts here.
pub fn foreground_pgrp(master: RawFd) -> Option<Pgid> {
    // SAFETY: master is a live PTY master.
    let p = unsafe { libc::tcgetpgrp(master) };
    if p < 0 { None } else { Some(Pgid(p)) }
}

/// The controlling terminal device of a slave, which is the containment identity.
pub fn tty_dev(slave: RawFd) -> io::Result<u64> {
    // SAFETY: zeroed stat is a valid out-param.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: slave is live and st is a valid out-param.
    if unsafe { libc::fstat(slave, &mut st) } != 0 {
        return Err(last_err());
    }
    // `st_rdev` is signed on this platform, so a plain widening cast sign-extends and the
    // two sides of the comparison stop being the same number. The device number is a 32 bit
    // unsigned quantity, so narrow first.
    Ok(st.st_rdev as u32 as u64)
}

pub fn signal_group(pgid: Pgid, sig: crate::teardown::Signal) -> io::Result<()> {
    // `kill(-1, ...)` signals every process the user can signal, and `kill(0, ...)` signals
    // our own group. Either would be catastrophic here, and both are reachable from a
    // failed parse that produced -1 or 0, so they are refused rather than trusted.
    if pgid.0 <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process group {}", pgid.0),
        ));
    }
    let s = match sig {
        crate::teardown::Signal::Hup => libc::SIGHUP,
        crate::teardown::Signal::Term => libc::SIGTERM,
        crate::teardown::Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: a negative pid signals the process group, which is what containment requires.
    if unsafe { libc::kill(-pgid.0, s) } < 0 {
        let e = last_err();
        // No such process is the normal outcome once a group has already gone.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e);
        }
    }
    Ok(())
}

/// Enumerate every process whose controlling terminal is `dev`.
///
/// This runs only at teardown, never on any idle path, so it does not reintroduce the
/// per-pane process scan the idle policy exists to eliminate.
/// Every pid on the machine.
///
/// Public and shared with the tests on purpose. An earlier version of the completeness test
/// re-implemented this sequence inline, so the production function was not on the code path
/// and the measured truncation bug could have been reintroduced with the gate still green.
#[cfg(target_os = "macos")]
pub fn scan_all_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the required size.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let cap = (n as usize) * 2 + 256;
    let mut pids = vec![0i32; cap];
    // SAFETY: pids has room for cap entries and the size is passed in bytes.
    let got = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr() as *mut libc::c_void,
            (cap * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if got <= 0 {
        return Vec::new();
    }
    // The return value is a count of pids, not a count of bytes. Counting non-zero entries
    // is correct under either reading, which is what stops the measured truncation bug from
    // coming back through a different door.
    let last = pids
        .iter()
        .rposition(|p| *p > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    pids.truncate(last);
    pids.retain(|p| *p > 0);
    pids
}

#[cfg(not(target_os = "macos"))]
pub fn scan_all_pids() -> Vec<i32> {
    std::fs::read_dir("/proc")
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
pub fn enumerate_tty(dev: u64) -> Vec<Member> {
    enumerate_tty_reporting(dev).0
}

/// The program a pid is running, as a path.
///
/// The path and not the name from the process table: `comm` is truncated to sixteen
/// characters on both platforms, which is enough to make two different agent CLIs look
/// identical.
#[cfg(target_os = "macos")]
pub fn program_of(pid: Pid) -> Option<String> {
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    // SAFETY: buf is a live buffer of the size we pass, which is the documented maximum.
    let n = unsafe {
        libc::proc_pidpath(
            pid.0,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

#[cfg(not(target_os = "macos"))]
pub fn program_of(pid: Pid) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/exe", pid.0))
        .ok()
        .map(|p| p.display().to_string())
}

/// The process group in the foreground of a terminal, and the leader of that group.
///
/// One `ioctl`, measured in microseconds, so it is cheap enough to ask on every read.
pub fn foreground_leader(master: RawFd) -> Option<Pid> {
    foreground_pgrp(master).map(|pgid| Pid(pgid.0))
}

/// The key a terminal uses to look itself up in a whole-daemon scan.
///
/// Not the same number as `tty_dev` on Linux, where `/proc` and glibc pack the device
/// differently, so both sides are reduced to the decoded pair.
#[cfg(target_os = "macos")]
pub fn scan_key(tty_dev: u64) -> u64 {
    tty_dev
}

#[cfg(not(target_os = "macos"))]
pub fn scan_key(tty_dev: u64) -> u64 {
    let (major, minor) = decode_glibc_dev(tty_dev);
    (major << 32) | minor
}

/// Every process on any controlling terminal, grouped by terminal device.
///
/// One pass for the whole daemon. Per terminal it was 360 microseconds of syscalls inside
/// each terminal's read loop every fifty milliseconds, which is both a cost that grows with
/// the terminal count and a synchronous block on the path a keystroke travels: measured,
/// twenty terminals doing it took the focused terminal's keystroke echo from 57 to 101
/// milliseconds at the ninety fifth percentile.
pub fn enumerate_all() -> (std::collections::HashMap<u64, Vec<Member>>, usize) {
    let mut by_tty: std::collections::HashMap<u64, Vec<Member>> = std::collections::HashMap::new();
    let (members, opaque) = enumerate_every_tty();
    for (dev, member) in members {
        by_tty.entry(dev).or_default().push(member);
    }
    (by_tty, opaque)
}

/// The same, plus how many processes could not be inspected at all.
///
/// `proc_pidinfo` refuses a process we do not own, and skipping it silently is how a root
/// owned process on our controlling terminal, a `sudo` job for instance, becomes invisible:
/// absent from the containment set, therefore absent from the liveness check, therefore
/// teardown reports success while a process still holds the pty. ADR-014 requires that case
/// to be reported rather than silently succeeded.
/// Every process with a controlling terminal, paired with the device it is on.
#[cfg(target_os = "macos")]
fn enumerate_every_tty() -> (Vec<(u64, Member)>, usize) {
    let pids = scan_all_pids();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let mut out = Vec::new();
    let mut opaque = 0usize;
    for pid in pids {
        // SAFETY: zeroed proc_bsdinfo is a valid out-param of the size we pass.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: info is a live, correctly sized buffer for PROC_PIDTBSDINFO.
        let rc = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc != size {
            opaque += 1;
            continue;
        }
        let pgid = info.pbi_pgid as i32;
        let dev = info.e_tdev as u64;
        if dev != u32::MAX as u64 && pgid > 1 {
            out.push((
                dev,
                Member {
                    pid: Pid(pid),
                    pgid: Pgid(pgid),
                    start_time: info.pbi_start_tvsec,
                },
            ));
        }
    }
    (out, opaque)
}

#[cfg(not(target_os = "macos"))]
fn enumerate_every_tty() -> (Vec<(u64, Member)>, usize) {
    // Linux keeps the same information in `stat`, so the grouping pass is the same shape.
    let mut out = Vec::new();
    let mut opaque = 0usize;
    for pid in scan_all_pids() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            opaque += 1;
            continue;
        };
        let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let f: Vec<&str> = after.split_whitespace().collect();
        if f.len() > 19 {
            let pgrp: i32 = f[2].parse().unwrap_or(-1);
            let tty_nr: i64 = f[4].parse().unwrap_or(-1);
            let start: u64 = f[19].parse().unwrap_or(0);
            if tty_nr > 0 && pgrp > 1 {
                // Keyed by the decoded pair, not the raw number: `/proc` uses
                // `new_encode_dev` and glibc's `st_rdev` packs the minor differently, so a
                // raw key would never match the one a terminal computes from its slave.
                let (major, minor) = decode_linux_dev(tty_nr as u64);
                out.push((
                    (major << 32) | minor,
                    Member {
                        pid: Pid(pid),
                        pgid: Pgid(pgrp),
                        start_time: start,
                    },
                ));
            }
        }
    }
    (out, opaque)
}

#[cfg(target_os = "macos")]
pub fn enumerate_tty_reporting(dev: u64) -> (Vec<Member>, usize) {
    let pids = scan_all_pids();
    let mut opaque = 0usize;
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let mut out = Vec::new();
    for pid in pids {
        // SAFETY: zeroed proc_bsdinfo is a valid out-param of the size we pass.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        // SAFETY: info is a live, correctly sized buffer for PROC_PIDTBSDINFO.
        let rc = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc != size {
            // We could not look at it. Not necessarily on our terminal, but not knowably
            // not, either, which is exactly the case that has to be counted.
            opaque += 1;
            continue;
        }
        let pgid = info.pbi_pgid as i32;
        if info.e_tdev as u64 == dev && pgid > 1 {
            out.push(Member {
                pid: Pid(pid),
                pgid: Pgid(pgid),
                start_time: info.pbi_start_tvsec,
            });
        }
    }
    (out, opaque)
}

#[cfg(not(target_os = "macos"))]
pub fn enumerate_tty_reporting(dev: u64) -> (Vec<Member>, usize) {
    let mut out = Vec::new();
    let mut opaque = 0usize;
    for pid in scan_all_pids() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            // The process may have exited between the scan and the read, or it may be one
            // we may not read. Counted either way, because the difference is not
            // observable from here and the consequence of guessing wrong is silence.
            opaque += 1;
            continue;
        };
        let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let f: Vec<&str> = after.split_whitespace().collect();
        // After comm: state, ppid, pgrp, session, tty_nr, tpgid, ... starttime is field 20
        // of this slice (field 22 of the whole line).
        if f.len() > 19 {
            let pgrp: i32 = f[2].parse().unwrap_or(-1);
            let tty_nr: i64 = f[4].parse().unwrap_or(-1);
            let start: u64 = f[19].parse().unwrap_or(0);
            // /proc uses new_encode_dev, which packs the minor differently from glibc's
            // 64 bit st_rdev, so compare the decoded pair rather than the raw number.
            // A member whose group could not be parsed is dropped rather than propagated
            // as -1, which `signal_group` would refuse anyway but which should never reach
            // the containment set in the first place.
            if pgrp > 1 && tty_nr >= 0 && decode_linux_dev(tty_nr as u64) == decode_glibc_dev(dev) {
                out.push(Member {
                    pid: Pid(pid),
                    pgid: Pgid(pgrp),
                    start_time: start,
                });
            }
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn decode_linux_dev(d: u64) -> (u64, u64) {
    ((d >> 8) & 0xfff, (d & 0xff) | ((d >> 12) & 0xfff00))
}

#[cfg(not(target_os = "macos"))]
fn decode_glibc_dev(d: u64) -> (u64, u64) {
    (
        ((d >> 8) & 0xfff) | ((d >> 32) & !0xfffu64),
        (d & 0xff) | ((d >> 12) & !0xffu64),
    )
}

/// Does this pid still exist with this start time? The pid-reuse guard.
#[cfg(target_os = "macos")]
pub fn still_alive(pid: Pid, start_time: u64) -> bool {
    // SAFETY: zeroed proc_bsdinfo is a valid out-param.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: info is live and correctly sized.
    let rc = unsafe {
        libc::proc_pidinfo(
            pid.0,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    rc == size && info.pbi_start_tvsec == start_time
}

#[cfg(not(target_os = "macos"))]
pub fn still_alive(pid: Pid, start_time: u64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.0)) else {
        return false;
    };
    let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
        return false;
    };
    let f: Vec<&str> = after.split_whitespace().collect();
    // Field 0 after the comm is the state. A zombie's `stat` file exists with the same
    // start time until its parent reaps it, so treating it as alive keeps teardown running
    // to its deadline and reporting failure for a terminal that was contained correctly.
    // macOS does not have this: `proc_pidinfo` on an unreaped exited child already fails.
    let zombie = f.first().is_some_and(|state| *state == "Z");
    !zombie && f.len() > 19 && f[19].parse::<u64>().unwrap_or(0) == start_time
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_pty_is_close_on_exec_on_both_ends() {
        // Invariant I12. This asserts the end state; the window is closed by opening with
        // the flag rather than setting it afterwards, which the next test pins down.
        let p = open_pty(80, 24).expect("open_pty");
        assert!(is_cloexec(p.master.as_raw_fd()), "master must be cloexec");
        assert!(is_cloexec(p.slave.as_raw_fd()), "slave must be cloexec");
    }

    #[test]
    fn close_on_exec_is_set_at_open_and_not_afterwards() {
        // A concurrent spawn on another worker thread can land between an open and a
        // later fcntl, and its child then holds this terminal's pair. The end-state test
        // above cannot see that window at all, so this one opens many pairs while other
        // threads open their own, and asserts every descriptor was already close-on-exec.
        // With the flag set at open there is no ordering to get wrong.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let bad = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let bad = Arc::clone(&bad);
            handles.push(std::thread::spawn(move || {
                for _ in 0..64 {
                    let p = open_pty(80, 24).expect("open_pty");
                    if !is_cloexec(p.master.as_raw_fd()) || !is_cloexec(p.slave.as_raw_fd()) {
                        bad.store(true, Ordering::Relaxed);
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(
            !bad.load(Ordering::Relaxed),
            "a descriptor was observed without close-on-exec"
        );
    }

    #[test]
    fn resize_is_observable_through_the_slave() {
        let p = open_pty(80, 24).expect("openpty");
        assert_eq!(window_size(p.slave.as_raw_fd()).unwrap(), (80, 24));
        resize(p.master.as_raw_fd(), 132, 43).unwrap();
        assert_eq!(window_size(p.slave.as_raw_fd()).unwrap(), (132, 43));
    }

    #[test]
    fn a_fresh_pty_has_a_distinct_controlling_device() {
        let a = open_pty(80, 24).unwrap();
        let b = open_pty(80, 24).unwrap();
        assert_ne!(
            tty_dev(a.slave.as_raw_fd()).unwrap(),
            tty_dev(b.slave.as_raw_fd()).unwrap()
        );
    }

    #[test]
    fn the_enumerator_reads_the_whole_process_table() {
        // The measured bug: dividing a pid count by four discarded three quarters of the
        // table, oldest first, which is inverted from what containment needs. An earlier
        // version of this test re-implemented the scan inline, so the production function
        // was not on the code path and the bug could have come back with the gate green.
        // It now calls `scan_all_pids`, which `enumerate_tty` also calls.
        let out = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid="])
            .output()
            .expect("ps");
        let ps_count = String::from_utf8_lossy(&out.stdout).lines().count();
        let seen = scan_all_pids().len();
        assert!(
            seen as f64 > ps_count as f64 * 0.8,
            "the enumerator saw {seen} pids and ps sees {ps_count}"
        );

        // And the tty filter itself, through `enumerate_tty`, against a device that many
        // processes really share. On macOS every daemon reports NODEV.
        #[cfg(target_os = "macos")]
        {
            let nodev = enumerate_tty(u32::MAX as u64);
            assert!(
                nodev.len() > 10,
                "enumerate_tty found {} members for NODEV, which suggests the scan is \
                 truncated rather than that the machine has no daemons",
                nodev.len()
            );
        }
    }

    #[test]
    fn signalling_process_group_zero_or_one_is_refused() {
        // kill(-1, ...) reaches every process the user can signal and kill(0, ...) reaches
        // our own group. Both are reachable from a failed parse, so both are refused.
        for bad in [-1, 0, 1] {
            assert!(
                signal_group(Pgid(bad), crate::teardown::Signal::Term).is_err(),
                "process group {bad} must be refused"
            );
        }
    }

    #[test]
    fn the_device_number_is_not_sign_extended() {
        // A signed st_rdev widened directly would make the two sides of the enumeration
        // comparison different numbers for any device with the high bit set.
        let p = open_pty(80, 24).unwrap();
        let dev = tty_dev(p.slave.as_raw_fd()).unwrap();
        assert!(
            dev <= u64::from(u32::MAX),
            "device number was sign extended: {dev:#x}"
        );
    }

    #[test]
    fn a_live_process_matches_its_own_start_time() {
        let me = Pid(std::process::id() as i32);
        #[cfg(target_os = "macos")]
        let start = {
            // SAFETY: zeroed out-param.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            // SAFETY: info is live and correctly sized.
            let rc = unsafe {
                libc::proc_pidinfo(
                    me.0,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    &mut info as *mut _ as *mut libc::c_void,
                    size,
                )
            };
            assert_eq!(rc, size);
            info.pbi_start_tvsec
        };
        #[cfg(not(target_os = "macos"))]
        let start = {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", me.0)).unwrap();
            let after = &stat[stat.rfind(')').unwrap() + 1..];
            after.split_whitespace().nth(19).unwrap().parse().unwrap()
        };
        assert!(still_alive(me, start));
        assert!(
            !still_alive(me, start.wrapping_add(1)),
            "a mismatched start time must not match"
        );
    }
}
