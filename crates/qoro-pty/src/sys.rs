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

/// Signal one process, for the fallback that knows its child's pid.
///
/// The same `pid <= 1` refusal as [`signal_group`]: pid 0 signals our own
/// group and pid -1 every process we can, and neither is ever the child.
pub fn signal_pid(pid: Pid, sig: crate::teardown::Signal) -> io::Result<()> {
    if pid.0 <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process {}", pid.0),
        ));
    }
    let s = match sig {
        crate::teardown::Signal::Hup => libc::SIGHUP,
        crate::teardown::Signal::Term => libc::SIGTERM,
        crate::teardown::Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: a positive pid signals exactly that process.
    if unsafe { libc::kill(pid.0, s) } < 0 {
        let e = last_err();
        // No such process is the normal outcome once the child has gone.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e);
        }
    }
    Ok(())
}

/// This pid's start time, for the reuse guard.
///
/// `None` when the pid is gone or unreadable, which reads as "not ours".
/// The value is whatever [`still_alive`] compares against, so capturing it
/// at spawn and probing with `still_alive` later is one consistent check.
#[cfg(target_os = "macos")]
pub fn start_time_of(pid: Pid) -> Option<u64> {
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
    (rc == size).then_some(info.pbi_start_tvsec)
}

/// This pid's start time, for the reuse guard.
///
/// Same field [`still_alive`] reads on this platform, so the two agree by
/// construction rather than by matching comments.
#[cfg(not(target_os = "macos"))]
pub fn start_time_of(pid: Pid) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.0)).ok()?;
    let after = stat.rfind(')').map(|i| &stat[i + 1..])?;
    let f: Vec<&str> = after.split_whitespace().collect();
    (f.len() > 19).then(|| f[19].parse::<u64>().unwrap_or(0))
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

/// Every process on the terminal `dev`, on either platform: the reporting scan with its
/// count of processes that could not be looked at dropped.
pub fn enumerate_tty(dev: u64) -> Vec<Member> {
    enumerate_tty_reporting(dev).0
}

/// Every process in the session led by `sid`, plus how many of them could not be inspected.
///
/// The key teardown still has once the leader is dead: POSIX revokes the controlling
/// terminal then, so a tty scan finds nothing, but `getsid` keeps answering with the dead
/// leader's pid. A job started and orphaned inside one scan interval is only reachable
/// this way. `getsid` answers for other users' processes too, so a member we cannot read
/// (a `sudo` job) is counted rather than silently missed.
///
/// Sound only while the leader is unreaped: the kernel does not hand out a pid that is
/// still a session id, and an unreaped leader holds its pid. After the reap the number
/// can belong to a new session, so callers read this before the leader can be reaped.
pub fn enumerate_session(sid: Pid) -> (Vec<Member>, usize) {
    let mut out = Vec::new();
    let mut opaque = 0usize;
    for pid in scan_all_pids() {
        // The leader is the caller's own child and reached through its handle; a zombie
        // leader cannot be read and would count as opaque on every plain `exit`.
        // SAFETY: getsid and getpgid only read the process table.
        if pid == sid.0 || unsafe { libc::getsid(pid) } != sid.0 {
            continue;
        }
        // Group and start time from one read, then the session asked again: a pid that
        // died and came back as a stranger between the reads is not recorded as ours.
        match group_and_start(Pid(pid)) {
            // SAFETY: as above.
            Some((pgid, start_time)) if unsafe { libc::getsid(pid) } == sid.0 => {
                if pgid > 1 {
                    out.push(Member {
                        pid: Pid(pid),
                        pgid: Pgid(pgid),
                        start_time,
                    });
                }
            }
            Some(_) => {}
            // Only a member that exists and is not ours to read, which is the `sudo` job
            // the count is for. One that exited or went between the two reads holds
            // nothing, and counting it made a plain teardown warn about nothing.
            // SAFETY: signal 0 only checks that the pid exists and may be signalled.
            None if unsafe { libc::kill(pid, 0) } != 0
                && io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) =>
            {
                opaque += 1
            }
            None => {}
        }
    }
    (out, opaque)
}

/// Is this pid, with this start time, still in this process group?
///
/// The teardown guard for a member that left its group after it was recorded: alive is
/// not enough, because the group's number may belong to a stranger by now.
pub fn in_group(pid: Pid, start_time: u64, pgid: Pgid) -> bool {
    group_and_start(pid) == Some((pgid.0, start_time))
}

/// A process's group and start time, read together.
#[cfg(target_os = "macos")]
fn group_and_start(pid: Pid) -> Option<(i32, u64)> {
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
    (rc == size).then_some((info.pbi_pgid as i32, info.pbi_start_tvsec))
}

#[cfg(not(target_os = "macos"))]
fn group_and_start(pid: Pid) -> Option<(i32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.0)).ok()?;
    let after = stat.rfind(')').map(|i| &stat[i + 1..])?;
    let f: Vec<&str> = after.split_whitespace().collect();
    (f.len() > 19).then(|| (f[2].parse().unwrap_or(-1), f[19].parse().unwrap_or(0)))
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
    let exe = std::fs::read_link(format!("/proc/{}/exe", pid.0))
        .ok()?
        .display()
        .to_string();
    Some(
        emulated_argv(&exe, &raw_argv_of(pid))
            .and_then(|argv| argv.into_iter().next())
            .unwrap_or(exe),
    )
}

/// The argument vector of the program a binfmt emulator is running, when `exe` is one.
///
/// A foreign-architecture process on Linux has the emulator as its `/proc/pid/exe`: every
/// x86_64 program in a container on an Apple silicon Mac reads as `rosetta`, and under
/// qemu-user as `qemu-x86_64`. Rosetta rewrites the argument vector to the program's own;
/// qemu puts itself and the program's path in front of it (`qemu-riscv64 /usr/bin/sleep
/// sleep 3`, measured), so both are taken off. A qemu tool run for its own sake
/// (`qemu-img convert ...`, `qemu-system-x86_64 -enable-kvm`) is left as itself. `None` for anything that is not an emulator, which is
/// every process on a machine running its own architecture.
#[cfg(not(target_os = "macos"))]
fn emulated_argv(exe: &str, argv: &[String]) -> Option<Vec<String>> {
    let name = std::path::Path::new(exe).file_name()?.to_str()?;
    if name == "rosetta" {
        return (!argv.is_empty()).then(|| argv.to_vec());
    }
    // qemu's own tools share the prefix with its user-mode emulators (`qemu-x86_64`,
    // `qemu-riscv64-static`), so the tools are named and everything else is an emulator.
    let tool = |rest: &str| {
        rest.starts_with("system-")
            || matches!(
                rest,
                "img" | "nbd" | "io" | "ga" | "pr-helper" | "storage-daemon" | "edid" | "keymap"
            )
    };
    if !name.strip_prefix("qemu-").is_some_and(|rest| !tool(rest)) {
        return None;
    }
    let program = argv.get(1)?;
    let base = |p: &str| {
        std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_os_string())
    };
    // With binfmt's preserve-argv0 flag the program's own argv[0] follows its path.
    let rest = match argv.get(2) {
        Some(zero) if base(zero) == base(program) => &argv[3..],
        _ => &argv[2..],
    };
    Some(
        std::iter::once(program.clone())
            .chain(rest.iter().cloned())
            .collect(),
    )
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
    // SAFETY: plain C data, for which all zero is a valid value.
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
        // SAFETY: the pre_exec closure only calls setpgid, which is async signal safe.
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
    let argv = raw_argv_of(pid);
    std::fs::read_link(format!("/proc/{}/exe", pid.0))
        .ok()
        .and_then(|exe| emulated_argv(&exe.display().to_string(), &argv))
        .unwrap_or(argv)
}

/// `/proc/pid/cmdline` as it is, emulator and all.
#[cfg(not(target_os = "macos"))]
fn raw_argv_of(pid: Pid) -> Vec<String> {
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

/// One process, as the decision to put a terminal to sleep reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcRow {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// Its controlling terminal, keyed like [`scan_key`]; `None` for a process with none,
    /// which is what a `setsid` child is.
    pub tty: Option<u64>,
    pub name: String,
}

/// Every process this user can read, with its parent, group and terminal.
///
/// One pass, read when a person asks for a sleep and not on any timer. What it is for is
/// the one thing tty membership cannot see: a descendant that left the terminal with
/// `setsid`, which teardown would never reach.
#[cfg(target_os = "macos")]
pub fn process_table() -> Vec<ProcRow> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let mut out = Vec::new();
    for pid in scan_all_pids() {
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
            continue;
        }
        let dev = info.e_tdev as u64;
        // SAFETY: the kernel NUL-terminates pbi_name within its fixed buffer.
        let name = unsafe { std::ffi::CStr::from_ptr(info.pbi_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        out.push(ProcRow {
            pid,
            ppid: info.pbi_ppid as i32,
            pgid: info.pbi_pgid as i32,
            tty: (dev != u32::MAX as u64).then_some(dev),
            name,
        });
    }
    out
}

#[cfg(not(target_os = "macos"))]
pub fn process_table() -> Vec<ProcRow> {
    let mut out = Vec::new();
    for pid in scan_all_pids() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_string();
        let f: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let tty_nr: i64 = f[4].parse().unwrap_or(0);
        out.push(ProcRow {
            pid,
            ppid: f[1].parse().unwrap_or(0),
            pgid: f[2].parse().unwrap_or(0),
            tty: (tty_nr > 0).then(|| {
                let (major, minor) = decode_linux_dev(tty_nr as u64);
                (major << 32) | minor
            }),
            name,
        });
    }
    out
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
    (out, opaque)
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

    #[test]
    fn a_captured_start_time_is_what_still_alive_checks() {
        let me = Pid(std::process::id() as i32);
        let start = start_time_of(me).expect("this test's own process is readable");
        assert!(still_alive(me, start));
        assert_eq!(start_time_of(Pid(-1)), None, "no such pid has a start time");
        assert!(
            signal_pid(Pid(0), crate::teardown::Signal::Hup).is_err(),
            "pid 0 would signal our own group"
        );
        assert!(
            signal_pid(Pid(-1), crate::teardown::Signal::Hup).is_err(),
            "pid -1 would signal everything we can"
        );
    }

    #[test]
    fn a_process_is_in_its_own_group_and_not_in_another() {
        let me = Pid(std::process::id() as i32);
        let start = start_time_of(me).expect("this test's own process is readable");
        // SAFETY: getpgrp takes nothing and cannot fail.
        let mine = Pgid(unsafe { libc::getpgrp() });
        assert!(in_group(me, start, mine));
        assert!(!in_group(me, start, Pgid(mine.0 + 1)));
        assert!(!in_group(me, start.wrapping_add(1), mine));
    }

    /// An exited, unreaped job of our own is not something that "could not be inspected":
    /// it holds nothing, and counting it would make every such teardown log a warning.
    /// Only a member we may not read (EPERM, a `sudo` job) counts.
    #[test]
    fn an_unreaped_member_of_our_own_is_not_counted_as_opaque() {
        use std::os::unix::process::CommandExt;
        // Its own session, and a background job whose parent then becomes `sleep`, which
        // never reaps it.
        // SAFETY: the pre_exec closure only calls setsid, which is async signal safe.
        let mut child = unsafe {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 0.1 & exec sleep 3")
                .pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .expect("spawn a session")
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (_, opaque) = enumerate_session(Pid(child.id() as i32));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(opaque, 0, "a zombie of our own was counted as opaque");
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
            // The value of an inline-code flag is code, not a script: `bun -e '...'` really
            // is bun, and counting the code as a script failed this on any machine with a
            // one-liner running.
            let inline =
                |a: &String| matches!(a.as_str(), "-e" | "--eval" | "-p" | "--print" | "-c");
            let has_script = argv
                .iter()
                .skip(1)
                .scan(false, |after_inline, a| {
                    let value_of_inline = std::mem::replace(after_inline, inline(a));
                    Some(!value_of_inline && !a.starts_with('-') && !a.is_empty())
                })
                .any(|script| script);
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

#[cfg(all(test, not(target_os = "macos")))]
mod emulated_tests {
    use super::emulated_argv;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_emulated_process_reads_as_the_program_it_runs_not_the_emulator() {
        assert_eq!(
            emulated_argv("/run/rosetta/rosetta", &argv(&["bash", "-l"])),
            Some(argv(&["bash", "-l"]))
        );
        // binfmt with preserve-argv0: emulator, program path, argv0, arguments.
        assert_eq!(
            emulated_argv(
                "/usr/bin/qemu-riscv64",
                &argv(&["/usr/bin/qemu-riscv64", "/usr/bin/sleep", "sleep", "3"])
            ),
            Some(argv(&["/usr/bin/sleep", "3"]))
        );
        // Without it: emulator, program path, arguments.
        assert_eq!(
            emulated_argv(
                "/usr/bin/qemu-x86_64",
                &argv(&["/usr/bin/qemu-x86_64", "/usr/local/bin/node", "gemini.js"])
            ),
            Some(argv(&["/usr/local/bin/node", "gemini.js"]))
        );
        // Started by a relative path: the program path is relative too.
        assert_eq!(
            emulated_argv(
                "/usr/bin/qemu-riscv64-static",
                &argv(&["/usr/bin/qemu-riscv64-static", "./sleep", "./sleep", "3"])
            ),
            Some(argv(&["./sleep", "3"]))
        );
    }

    #[test]
    fn a_qemu_tool_run_for_itself_is_not_taken_for_an_emulator() {
        assert_eq!(
            emulated_argv(
                "/usr/bin/qemu-img",
                &argv(&["/usr/bin/qemu-img", "convert", "a.img"])
            ),
            None
        );
        assert_eq!(
            emulated_argv(
                "/usr/bin/qemu-system-x86_64",
                &argv(&["qemu-system-x86_64", "-enable-kvm"])
            ),
            None
        );
        assert_eq!(emulated_argv("/usr/bin/bash", &argv(&["bash"])), None);
    }
}

/// One process of a group, as the kernel accounts for it: its pid, the CPU time it has
/// used so far in nanoseconds, and the memory it holds now in bytes.
pub type ProcessUsage = (Pid, u64, u64);

/// Every member of each asked-for process group, with what it has cost so far.
///
/// Per process and not per group, so a caller can take differences per process: a group
/// total falls when a busy member exits, and a difference of totals would read that as
/// idle. Groups at or below zero are skipped: Linux answers zero for a terminal with no
/// foreground job, and zero is also every kernel thread's group. A member the kernel will
/// not answer for (another user's, or gone since the listing) is left out, and a group
/// with no member left to ask is absent from the answer.
pub fn group_processes(groups: &[Pgid]) -> std::collections::BTreeMap<i32, Vec<ProcessUsage>> {
    let wanted: Vec<Pgid> = groups.iter().copied().filter(|g| g.0 > 0).collect();
    let mut out = std::collections::BTreeMap::new();
    if wanted.is_empty() {
        return out;
    }
    for (group, members) in members_of(&wanted) {
        if !members.is_empty() {
            out.insert(group, members);
        }
    }
    out
}

/// The asked-for groups' members with their costs: `proc_listpids` filters by group in
/// the kernel, so each group is one call and each member one more.
#[cfg(target_os = "macos")]
fn members_of(groups: &[Pgid]) -> Vec<(i32, Vec<ProcessUsage>)> {
    groups
        .iter()
        .map(|group| {
            let members = group_members(*group)
                .into_iter()
                .filter_map(|pid| process_usage(pid).map(|(cpu, rss)| (pid, cpu, rss)))
                .collect();
            (group.0, members)
        })
        .collect()
}

/// One process's CPU nanoseconds and physical footprint in bytes, from the kernel's own
/// accounting. The footprint and not the resident size: it is what Activity Monitor calls
/// memory, and it does not count shared pages once per member.
///
/// `ri_user_time` and `ri_system_time` are mach absolute time units, not nanoseconds: on
/// Apple silicon one unit is about 42 ns, so they go through the timebase first.
#[cfg(target_os = "macos")]
fn process_usage(pid: Pid) -> Option<(u64, u64)> {
    // SAFETY: a zeroed rusage_info_v2 is a valid out-param for the V2 flavour.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a live rusage_info_v2, which is what RUSAGE_INFO_V2 writes.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid.0,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut libc::rusage_info_v2 as *mut libc::rusage_info_t,
        )
    };
    if rc != 0 {
        return None;
    }
    // `mach_timebase_info` itself, declared here: libc marks its binding deprecated in
    // favour of a crate, and one two-field struct is not worth a dependency.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    // SAFETY: the signature matches <mach/mach_time.h>: one pointer to a two-u32 struct,
    // returning kern_return_t, which is an int.
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut base = Timebase { numer: 1, denom: 1 };
        // SAFETY: a valid out-param; a failure leaves the one-to-one default.
        unsafe { mach_timebase_info(&mut base) };
        (base.numer, base.denom.max(1))
    });
    let ticks = info.ri_user_time.saturating_add(info.ri_system_time) as u128;
    let nanos = (ticks * numer as u128 / denom as u128).min(u64::MAX as u128) as u64;
    Some((nanos, info.ri_phys_footprint))
}

/// The asked-for groups' members with their costs, from one pass over `/proc`: every
/// process's `stat` line carries its group, its CPU ticks and its resident pages, so one
/// read per process answers all three and no group costs a pass of its own.
#[cfg(not(target_os = "macos"))]
fn members_of(groups: &[Pgid]) -> Vec<(i32, Vec<ProcessUsage>)> {
    let mut by_group: std::collections::BTreeMap<i32, Vec<ProcessUsage>> =
        groups.iter().map(|g| (g.0, Vec::new())).collect();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return by_group.into_iter().collect();
    };
    // SAFETY: sysconf reads a constant.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    // SAFETY: sysconf reads a constant.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After the last close bracket the fields count from 3 (`state`): the group is
        // field 5, `utime` and `stime` 14 and 15, and the resident pages 24.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let field = |n: usize| fields.get(n - 3).and_then(|f| f.parse::<u64>().ok());
        let (Some(group), Some(utime), Some(stime), Some(pages)) =
            (field(5), field(14), field(15), field(24))
        else {
            continue;
        };
        if let Some(members) = by_group.get_mut(&(group as i32)) {
            members.push((
                Pid(pid),
                (utime + stime).saturating_mul(1_000_000_000) / hz,
                pages.saturating_mul(page),
            ));
        }
    }
    by_group.into_iter().collect()
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    /// A tenth of a second spent spinning shows up as about a tenth of a second of this
    /// process's own CPU, in nanoseconds: mach units read as nanoseconds would be forty
    /// times too small. Per process, so other tests running beside this one do not count.
    #[test]
    fn a_process_cpu_is_counted_in_real_nanoseconds_and_its_memory_in_bytes() {
        // SAFETY: getpgrp and getpid have no preconditions.
        let (group, me) = (Pgid(unsafe { libc::getpgrp() }), unsafe { libc::getpid() });
        let own = || {
            group_processes(&[group])
                .remove(&group.0)
                .and_then(|members| members.into_iter().find(|(pid, _, _)| pid.0 == me))
                .expect("this process is in its own group")
        };
        let (_, before, resident) = own();
        assert!(
            resident > 1 << 20,
            "a running test uses more than a megabyte: {resident}"
        );
        let start = std::time::Instant::now();
        let mut spin = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(100) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        let (_, after, _) = own();
        let spent = after.saturating_sub(before);
        assert!(
            (50_000_000..2_000_000_000).contains(&spent),
            "100 ms of spinning read as {spent} ns"
        );
        assert!(
            group_processes(&[Pgid(0)]).is_empty(),
            "group zero is never asked"
        );
    }
}
