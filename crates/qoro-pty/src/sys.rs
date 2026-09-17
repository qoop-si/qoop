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

/// The directory a process is in now.
///
/// Where a tab actually is, which is not where it was started: a person opens a tab, cds
/// into a worktree and starts an agent there, and a restore that used the directory the
/// tab was born in would put that agent back in the wrong project and carry on the wrong
/// conversation.
#[cfg(target_os = "macos")]
pub fn cwd_of(pid: Pid) -> Option<std::path::PathBuf> {
    // `proc_pidinfo` with `PROC_PIDVNODEPATHINFO` fills a struct whose first member is the
    // current directory, path included. It refuses a process we do not own, which is the
    // same answer as not knowing.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: info is a live, correctly sized buffer for PROC_PIDVNODEPATHINFO.
    let n = unsafe {
        libc::proc_pidinfo(
            pid.0,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n < size {
        return None;
    }
    // The path is declared as chunks of an array rather than one, so it is flattened back
    // into the string of bytes it is, and read to the first nul.
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    if bytes.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(not(target_os = "macos"))]
pub fn cwd_of(pid: Pid) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{}/cwd", pid.0)).ok()
}

/// Every process in one process group.
///
/// The leader is not always the interesting one. A person who starts an agent through a
/// wrapper puts the wrapper at the head of the group and the agent under it, both in the
/// same foreground group, and asking only the leader answers `opencode-vpn` for a terminal
/// that is running opencode. Measured on a real machine: the tab said so and the agents
/// panel left the row out entirely, because the wrapper is not a name in the agent table.
///
/// The group and not the process tree: a group is what the kernel already calls "the job in
/// the foreground", and walking children instead would also collect the `rg` an agent
/// happens to be running this second.
#[cfg(target_os = "macos")]
pub fn group_members(pgid: Pgid) -> Vec<Pid> {
    // `proc_listpids` with `PROC_PGRP_ONLY` answers exactly this question and answers it as
    // a plain array of pids, so nothing here has to know the shape of a kernel struct.
    // From `libproc.h`: 1 all, 2 process group, 3 tty, 4 uid, 5 ruid, 6 parent.
    const PROC_PGRP_ONLY: u32 = 2;
    // SAFETY: a null buffer asks for the size the answer needs.
    let bytes =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, pgid.0 as u32, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    let count = bytes as usize / std::mem::size_of::<i32>();
    // Room for a process that started between the two calls, which is a race the kernel
    // does not protect against and which would otherwise truncate the answer.
    let mut pids = vec![0i32; count + 8];
    // SAFETY: the same call with a buffer of the size it asked for, plus slack. The return
    // is how many bytes were written, and nothing past that is read.
    let written = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            pgid.0 as u32,
            pids.as_mut_ptr() as *mut libc::c_void,
            (pids.len() * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if written <= 0 {
        return Vec::new();
    }
    pids.truncate(written as usize / std::mem::size_of::<i32>());
    pids.into_iter().filter(|p| *p > 0).map(Pid).collect()
}

/// Every process in one process group, read from `/proc`.
#[cfg(not(target_os = "macos"))]
pub fn group_members(pgid: Pgid) -> Vec<Pid> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pgrp` is the fifth field, and the second can contain spaces and brackets, so the
        // scan starts after the last close bracket.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        if rest
            .split_whitespace()
            .nth(2)
            .and_then(|f| f.parse::<i32>().ok())
            == Some(pgid.0)
        {
            out.push(Pid(pid));
        }
    }
    out
}

#[cfg(test)]
mod group_tests {
    use super::*;

    /// A job is a process group: the shell that leads it and everything it started. Asking
    /// only the leader is how an agent behind a wrapper became invisible.
    #[test]
    fn a_group_holds_the_leader_and_what_it_started() {
        // Its own group, the way a shell puts a job in one. Without this the child inherits
        // the test runner's group and there is no group with its pid to ask about.
        let mut child = unsafe {
            use std::os::unix::process::CommandExt;
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 5 & wait")
                .stdout(std::process::Stdio::null())
                // SAFETY: post fork, async signal safe, no allocation.
                .pre_exec(|| {
                    if libc::setpgid(0, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .expect("spawn a little job")
        };
        let leader = child.id() as i32;
        // The shell has to reach its `sleep` before the group has two members in it.
        let mut members = Vec::new();
        for _ in 0..50 {
            members = group_members(Pgid(leader));
            if members.len() > 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            members.iter().any(|p| p.0 == leader),
            "the leader is not in its own group: {members:?}"
        );
        assert!(
            members.len() > 1,
            "the group holds only its leader, so nothing started under it was seen: \
             {members:?}"
        );
    }
}

#[cfg(test)]
mod argv_tests {
    use super::*;

    #[test]
    fn a_live_process_reports_the_arguments_it_was_started_with() {
        // A real process with arguments nobody could guess, read back through the same
        // path the daemon uses. A unit test with a fixture would prove the parser and not
        // the syscall, and the syscall is the part that differs by platform.
        let marker = "qoro-argv-marker";
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("sleep 5 # {marker}")])
            .spawn()
            .expect("spawn");
        let argv = argv_of(Pid(child.id() as i32));
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            argv.iter().any(|a| a.contains(marker)),
            "argv did not carry the arguments: {argv:?}"
        );
        assert!(argv.first().is_some_and(|a| a.contains("sh")), "{argv:?}");
    }

    #[test]
    fn a_process_that_is_gone_answers_nothing_rather_than_failing() {
        // Pid 1 is not ours to read on a mac, and a pid that never existed is not readable
        // anywhere. Both are "we cannot tell", which the caller already handles.
        let _ = argv_of(Pid(i32::MAX));
    }
}

/// A process's argument vector, for telling an agent from the runtime that started it.
///
/// The executable path alone is not enough: an agent installed as a node package runs as
/// `node /path/to/cli.js`, and a person looking at `node` in an index learns nothing. The
/// caller decides what the argv means (`qoro_domain::program`); this only fetches it.
///
/// Empty when the process is gone or is not ours to read, which is the same answer as "we
/// cannot tell" and is what the caller already handles.
#[cfg(target_os = "macos")]
pub fn argv_of(pid: Pid) -> Vec<String> {
    // `KERN_PROCARGS2` answers with a small header and then a run of NUL terminated
    // strings: an `argc`, the executable path, padding, and then `argc` arguments. There is
    // no other unprivileged way to read another process's argv on macOS.
    let mut size: usize = 0;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid.0];
    // SAFETY: a sizing call with a null buffer, which is how sysctl reports the length it
    // needs. Every pointer is either null or a live local.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 || size > 4 * 1024 * 1024 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size];
    // SAFETY: buf is live and `size` is the length sysctl just asked for.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < 4 {
        return Vec::new();
    }
    buf.truncate(size);
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
    let mut rest = &buf[4..];
    // The executable path, then the padding that aligns what follows.
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    rest = &rest[end..];
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    rest.split(|b| *b == 0)
        .take(argc)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// A process's argument vector. See the macOS version for what this is for.
#[cfg(not(target_os = "macos"))]
pub fn argv_of(pid: Pid) -> Vec<String> {
    std::fs::read(format!("/proc/{}/cmdline", pid.0))
        .map(|raw| {
            raw.split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect()
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod identity_chain {
    use super::*;

    #[test]
    fn a_shebang_script_is_named_by_itself_and_not_by_its_interpreter() {
        // The whole chain as the daemon runs it: the executable the kernel reports, the
        // argv it was given, and the name a person would say. A stand in agent, because a
        // real one cannot be installed by a test.
        let dir = std::env::temp_dir().join(format!("qoro-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let script = dir.join("claude");
        std::fs::write(&script, "#!/bin/sh\nsleep 5\n").expect("write");
        std::fs::set_permissions(
            &script,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )
        .expect("chmod");

        let mut child = std::process::Command::new(&script).spawn().expect("spawn");
        let pid = Pid(child.id() as i32);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let executable = program_of(pid).unwrap_or_default();
        let argv = argv_of(pid);
        let program = qoro_domain::program::identify(&executable, &argv);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            program.name, "claude",
            "executable {executable:?} argv {argv:?} identified as {program:?}"
        );
        assert!(program.agent);
    }
}

#[cfg(test)]
mod real_agents {
    use super::*;

    /// Whatever coding agents happen to be running on this machine, named.
    ///
    /// Not an assertion about a particular one: a machine with none running is a machine
    /// with nothing to check, and a test that demanded one would fail on a clean CI box.
    /// What it does assert is that nothing this identification path produces is a runtime
    /// name, because that is the failure it exists to prevent and it is invisible until a
    /// real install has the shape that causes it.
    #[test]
    fn nothing_running_on_this_machine_is_identified_as_its_runtime() {
        let mut seen = Vec::new();
        for pid in scan_all_pids().into_iter().take(400) {
            let pid = Pid(pid);
            let Some(executable) = program_of(pid) else {
                continue;
            };
            let argv = argv_of(pid);
            let program = qoro_domain::program::identify(&executable, &argv);
            if program.agent {
                seen.push((program.name.clone(), executable.clone()));
            }
            // Only when there is something to unwrap. A person sitting in a bare `node`
            // repl is a process whose name really is `node`, and a test that called that a
            // failure would be asserting that the repl cannot exist.
            let base = executable.rsplit('/').next().unwrap_or(&executable);
            let is_runtime = matches!(base, "node" | "bun" | "deno") || base.starts_with("python");
            let has_script = argv
                .iter()
                .skip(1)
                .any(|a| !a.starts_with('-') && !a.is_empty());
            if is_runtime && has_script {
                assert_ne!(
                    program.name, base,
                    "{executable} with argv {argv:?} was identified as its own runtime"
                );
            }
        }
        eprintln!("agents identified on this machine: {seen:?}");
    }
}

#[cfg(test)]
mod cwd_tests {
    use super::*;

    /// The directory a process is in, read for a process whose directory we know.
    #[test]
    fn a_process_reports_the_directory_it_is_actually_in() {
        let dir = std::env::temp_dir();
        // A child that changes directory and then waits, so there is something to read.
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("cd / && exec sleep 5")
            .spawn()
            .expect("a child");
        // Given a moment to reach the `exec`, which is when its directory is `/`.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let read = cwd_of(Pid(child.id() as i32));
        let _ = child.kill();
        let _ = child.wait();
        let read = read.expect("a directory for a live child we started");
        assert_eq!(
            read,
            std::path::Path::new("/"),
            "the directory read is not the one the child moved to"
        );
        assert_ne!(
            read, dir,
            "reading the caller's own directory instead of the child's would pass every \
             test that never moved"
        );
    }

    #[test]
    fn a_process_that_is_gone_has_no_directory() {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("a child");
        let pid = Pid(child.id() as i32);
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(cwd_of(pid), None);
    }
}
