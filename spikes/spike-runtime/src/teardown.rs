//! Spike: independently verify five claims that contradict what ADR-014, ADR-002 and
//! the plan currently say. A measurement made elsewhere is a lead, not a fact, so each is
//! reproduced here from scratch.
//!
//! Claims under test:
//!   A. `kill(-pgid)` of a session leader does not reach a job that called
//!      `setpgid(0, 0)`, which is what every job-control shell does. If true, ADR-014's
//!      "containment identity is a session, no enumeration needed" is wrong.
//!   B. The reason an exiting child is not reapable is undrained tty output, not the
//!      parent holding the master open. If true, spike-proc finding 5 has the wrong cause
//!      and ADR-014's "release the master" step destroys the child's final output.
//!   C. After the child exits and the master is drained, `poll` reports POLLIN|POLLHUP
//!      and `read` returns 0 forever, which under AsyncFd is an infinite busy loop unless
//!      end of stream is handled explicitly.
//!   D. A large paste with no line terminator is silently discarded by the canonical-mode
//!      line discipline while `write` reports full success.
//!   E. Closing the master DOES let the child reap, but only by discarding its output.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn open_pty(cols: u16, rows: u16) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (mut m, mut s): (RawFd, RawFd) = (-1, -1);
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two valid fds on success; ws is fully initialised.
    let rc = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut ws,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both fds are fresh and owned by nobody else.
    unsafe { Ok((OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s))) }
}

fn set_nonblocking(fd: RawFd) {
    // SAFETY: fd is live.
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, f | libc::O_NONBLOCK);
    }
}

fn dup_stdio(fd: RawFd) -> std::io::Result<Stdio> {
    // SAFETY: fd is live; dup yields a fresh owned descriptor.
    let d = unsafe { libc::dup(fd) };
    if d < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: d is fresh and unowned.
    Ok(unsafe { Stdio::from(OwnedFd::from_raw_fd(d)) })
}

fn spawn_on(slave: RawFd, program: &str, args: &[&str]) -> std::io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: only async-signal-safe syscalls run post-fork, no allocation.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.stdin(dup_stdio(slave)?)
            .stdout(dup_stdio(slave)?)
            .stderr(dup_stdio(slave)?)
            .pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
    }
    cmd.env("TERM", "xterm-256color");
    cmd.spawn()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Is this pid a zombie, meaning it exited but has not been reaped?
fn is_zombie(pid: i32) -> bool {
    let out = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'),
        Err(_) => false,
    }
}

/// macOS reports a process that is stuck in exit as STAT "E".
fn is_exiting(pid: i32) -> bool {
    let out = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().contains('E'),
        Err(_) => false,
    }
}

fn drain(fd: RawFd) -> usize {
    let mut buf = [0u8; 65536];
    let mut total = 0;
    loop {
        // SAFETY: fd is live, buf is a valid writable slice.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    total
}

/// Enumerate every process whose controlling terminal is this PTY, which is what
/// ADR-014's containment needs.
///
/// Two corrections a second look forced. First, the constant is `KERN_PROC_SESSION`, not
/// `KERN_PROC_SID`, which does not exist. Second, `sysctl` with either selector returns
/// `kinfo_proc`, and the libc crate does not expose that type on Apple platforms, so the
/// sysctl route is not implementable from safe Rust without hand-declaring a large and
/// version-sensitive struct.
///
/// The route that IS implementable, and that this spike proves works, is
/// `proc_listallpids` plus `proc_pidinfo(PROC_PIDTBSDINFO)`, filtering on `e_tdev`, the
/// controlling terminal device. Every process in the terminal's session shares that
/// device, which makes it an equally good containment identity and one we can actually
/// read. It costs one full pid enumeration, which runs only at teardown, never on any
/// idle path.
#[cfg(target_os = "macos")]
fn tty_members(tty_dev: u64) -> Vec<(i32, i32)> {
    // SAFETY: a null buffer asks proc_listallpids for the required size in bytes.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // `proc_listallpids` returns the number of PIDS, not a number of bytes. A second
    // look caught this being divided by four, which silently discarded three quarters of the
    // table, and because the list comes back newest first the discarded quarter was the
    // oldest processes, which is exactly what containment needs. Verified directly: the
    // return value equals the index after the last non-zero entry and equals `ps -ax`.
    // The count of non-zero entries is used rather than the return value, so this is
    // correct under either interpretation.
    let cap = (n as usize) * 2 + 256;
    let mut pids = vec![0i32; cap];
    let got = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr() as *mut libc::c_void,
            (cap * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if got <= 0 {
        return Vec::new();
    }
    let last = pids
        .iter()
        .rposition(|p| *p > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    pids.truncate(last);

    let mut out = Vec::new();
    for pid in pids.into_iter().filter(|p| *p > 0) {
        // SAFETY: zeroed proc_bsdinfo is a valid out-param of the size we pass.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
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
        if rc == size && info.e_tdev as u64 == tty_dev {
            out.push((pid, info.pbi_pgid as i32));
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn tty_members(tty_dev: u64) -> Vec<(i32, i32)> {
    // Linux: /proc/<pid>/stat field 7 is the controlling terminal device number.
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
                continue;
            };
            let fields: Vec<&str> = after.split_whitespace().collect();
            // After the comm field: state, ppid, pgrp, session, tty_nr, ...
            if fields.len() > 4 {
                let pgrp: i32 = fields[2].parse().unwrap_or(-1);
                let tty_nr: i64 = fields[4].parse().unwrap_or(-1);
                // /proc stat field 7 uses new_encode_dev, which packs the minor number
                // differently from glibc's 64 bit st_rdev. Compare the decoded pair, not
                // the raw number, or the two agree only for minor numbers under 256.
                if tty_nr >= 0 && decode_linux_dev(tty_nr as u64) == decode_glibc_dev(tty_dev) {
                    out.push((pid, pgrp));
                }
            }
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn decode_linux_dev(d: u64) -> (u64, u64) {
    // new_encode_dev: major in bits 8..20, minor split across 0..8 and 20..32.
    let major = (d >> 8) & 0xfff;
    let minor = (d & 0xff) | ((d >> 12) & 0xfff00);
    (major, minor)
}

#[cfg(not(target_os = "macos"))]
fn decode_glibc_dev(d: u64) -> (u64, u64) {
    let major = ((d >> 8) & 0xfff) | ((d >> 32) & !0xfffu64);
    let minor = (d & 0xff) | ((d >> 12) & !0xffu64);
    (major, minor)
}

/// The controlling terminal device number of a PTY slave, which is what the members of
/// its session are matched against.
fn tty_dev_of(slave_fd: RawFd) -> u64 {
    // SAFETY: zeroed stat is a valid out-param and slave_fd is live.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::fstat(slave_fd, &mut st) } != 0 {
        return u64::MAX;
    }
    st.st_rdev as u64
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut failures: Vec<String> = Vec::new();

    // ================= Claim A: does kill(-pgid) reach a job in our session? ==========
    // Two variants, because the first attempt at this test was inconclusive: `sh -mc`
    // did not actually give the background job its own process group, so nothing was
    // being tested. Variant 1 creates the condition explicitly. Variant 2 checks whether
    // a real interactive shell creates it on its own, which is what decides whether this
    // matters in practice.
    {
        // A real interactive shell, its own session leader, starts a background job. Job
        // control gives that job its own process group INSIDE the leader's session, which
        // is the exact configuration ADR-014 has to contain.
        //
        // The first version of this test spawned the "escapee" from our own process, so it
        // was a sibling in our session rather than a member of the leader's. It still
        // showed that a pgid-scoped signal does not cross process groups, but it did not
        // model the case the ADR is about. A second look caught that.
        let (master, slave) = open_pty(80, 24)?;
        set_nonblocking(master.as_raw_fd());
        let tty_dev = tty_dev_of(slave.as_raw_fd());
        // Pinned rather than read from $SHELL: this assertion votes on the build, and a
        // runner's shell choice, interactive startup files and job-control notices are not
        // what is under test. bash is present on both supported platforms.
        let shell = if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash".to_string()
        } else {
            "/bin/sh".to_string()
        };
        let mut sh = spawn_on(slave.as_raw_fd(), &shell, &["-i"])?;
        drop(slave);
        let sh_pid = sh.id() as i32;
        std::thread::sleep(Duration::from_millis(500));
        drain(master.as_raw_fd());

        // SAFETY: master is live and the slice is valid for the call.
        unsafe {
            // nohup so the job ignores SIGHUP. Without this the shell relays a hangup to
            // its own jobs and the job dies for a reason that has nothing to do with our
            // signal, which is what the first version of this test actually measured.
            let cmd = b"nohup sleep 60 >/dev/null 2>&1 & echo JOBPID=$!\n";
            libc::write(
                master.as_raw_fd(),
                cmd.as_ptr() as *const libc::c_void,
                cmd.len(),
            );
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut acc = String::new();
        let mut job_pid = 0i32;
        while Instant::now() < deadline && job_pid == 0 {
            let mut buf = [0u8; 4096];
            // SAFETY: master live, buf valid.
            let n = unsafe {
                libc::read(
                    master.as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n > 0 {
                acc.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
                for part in acc.match_indices("JOBPID=") {
                    let rest = &acc[part.0 + 7..];
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if !digits.is_empty() && rest.len() > digits.len() {
                        job_pid = digits.parse().unwrap_or(0);
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if job_pid == 0 {
            failures.push("A1: could not start a background job in the shell".into());
        }

        // SAFETY: getpgid and getsid on pids we caused to exist.
        let (sh_pgid, sh_sid) = unsafe { (libc::getpgid(sh_pid), libc::getsid(sh_pid)) };
        let (job_pgid, job_sid) = if job_pid > 0 {
            // SAFETY: as above.
            unsafe { (libc::getpgid(job_pid), libc::getsid(job_pid)) }
        } else {
            (-1, -1)
        };
        println!(
            "[A1] shell {shell}: leader pid={sh_pid} pgid={sh_pgid} sid={sh_sid} | \
job pid={job_pid} pgid={job_pgid} sid={job_sid}"
        );
        let same_session = job_pid > 0 && job_sid == sh_sid && job_sid != -1;
        let own_group = job_pid > 0 && job_pgid != sh_pgid;
        println!(
            "     job is in the leader's session: {same_session}; job has its own process group: {own_group}; it ignores SIGHUP"
        );

        // Enumerate the containment set BEFORE anything is signalled.
        let before_members = tty_members(tty_dev);
        let before_found = before_members.iter().any(|(p, _)| *p == job_pid);
        println!(
            "[A2a] controlling-tty enumeration BEFORE teardown (dev={tty_dev}): {} members, job found={before_found}",
            before_members.len()
        );
        if job_pid > 0 && !before_found {
            failures.push(format!(
                "A2a: enumeration before teardown did not find job {job_pid}; the mechanism does not work at all"
            ));
        }

        // Run exactly the escalation the first draft of ADR-014 prescribed.
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGKILL] {
            // SAFETY: signalling a process group we created.
            unsafe { libc::kill(-sh_pgid, sig) };
            // Keep draining: claim B showed an exiting child is not reapable until its
            // tty output has been read, and an undrained leader reads as still alive.
            for _ in 0..12 {
                drain(master.as_raw_fd());
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = sh.try_wait();
        let leader_alive = alive(sh_pid) && !is_zombie(sh_pid) && !is_exiting(sh_pid);
        let job_alive = job_pid > 0 && alive(job_pid) && !is_zombie(job_pid);
        println!(
            "     after HUP/TERM/KILL on -{sh_pgid}: leader_alive={leader_alive} job_alive={job_alive} -> {}",
            if job_alive {
                "CLAIM A CONFIRMED: process-group teardown leaves the job running"
            } else {
                "CLAIM A REFUTED"
            }
        );
        if own_group && same_session && !job_alive {
            failures.push(
                "A1: a job in its own group died from a pgid-scoped signal, which contradicts POSIX"
                    .into(),
            );
        }
        if job_pid > 0 && !job_alive && own_group {
            failures
                .push("A1: could not demonstrate the escape, so the ADR fix is unproven".into());
        }

        // Now prove the PROPOSED fix: enumerate the session and signal every group in it.
        if job_alive {
            let after_members = tty_members(tty_dev);
            let after_found = after_members.iter().any(|(p, _)| *p == job_pid);
            println!(
                "[A2b] enumeration AFTER the leader died: {} members, job found={after_found}",
                after_members.len()
            );
            if before_found && !after_found {
                println!(
                    "      the controlling terminal is disassociated when the session leader dies, \
so the containment set must be a union across rounds and can never shrink"
                );
            }
            // The union across rounds is what the ADR must specify.
            let mut members = before_members.clone();
            members.extend(after_members.iter().copied());
            members.sort_unstable();
            members.dedup();
            let found = members.iter().any(|(p, _)| *p == job_pid);
            let mut groups: Vec<i32> = members.iter().map(|(_, g)| *g).collect();
            groups.sort_unstable();
            groups.dedup();
            println!(
                "[A2] union of enumerations: {} members, {} distinct groups, job found={found}",
                members.len(),
                groups.len()
            );
            for g in &groups {
                if *g > 0 {
                    // SAFETY: signalling groups we enumerated from our own session tree.
                    unsafe { libc::kill(-g, libc::SIGKILL) };
                }
            }
            std::thread::sleep(Duration::from_millis(300));
            let job_alive_now = alive(job_pid) && !is_zombie(job_pid);
            println!(
                "     after signalling every enumerated group: job_alive={job_alive_now} -> {}",
                if !job_alive_now {
                    "PROPOSED FIX WORKS: tty enumeration reaches what pgid signalling missed"
                } else {
                    "PROPOSED FIX FAILED"
                }
            );
            if !found {
                failures.push(format!(
                    "A2: tty enumeration did not find the job pid {job_pid}, so the ADR-014 mechanism is not implementable as written on this platform"
                ));
            }
            if job_alive_now {
                failures.push("A2: the job survived signalling every enumerated group".into());
                // SAFETY: last resort cleanup of a pid we caused to exist.
                unsafe { libc::kill(job_pid, libc::SIGKILL) };
            }
        }
        let _ = sh.kill();
        let _ = sh.try_wait();
        drop(master);
    }

    // ============ Claims B and E: what actually blocks the reap? ======================
    {
        // The child writes more than the master's buffer can absorb and then exits
        // cleanly. The parent holds the master and never reads.
        //
        // An earlier version wrote two bytes, and a run of it under load
        // saw it reap anyway. That is correct behaviour, not a flake in the kernel: a
        // small write is usually already sitting in the master's queue by the time the
        // slave closes, so there is nothing left to wait for. The blocking condition is
        // output that has NOT yet been absorbed, which needs a write larger than the
        // buffer. The design rule is unchanged and is required for output preservation
        // either way; the test is what had to become deterministic.
        let (master, slave) = open_pty(80, 24)?;
        set_nonblocking(master.as_raw_fd());
        let mut child = spawn_on(
            slave.as_raw_fd(),
            "/bin/sh",
            &[
                "-c",
                "i=0; while [ $i -lt 4096 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n'; i=$((i+1)); done; exit 3",
            ],
        )?;
        drop(slave);
        let _pid = child.id() as i32;

        let deadline = Instant::now() + Duration::from_secs(4);
        let mut reaped_without_drain = None;
        while Instant::now() < deadline {
            if let Ok(Some(st)) = child.try_wait() {
                reaped_without_drain = st.code();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "[B] exiting child with 256 KiB of undrained output, master held open, no reads for 4s: {}",
            match reaped_without_drain {
                Some(c) => format!("REAPED code={c} (claim B refuted)"),
                None => "NOT REAPED (claim B supported so far)".to_string(),
            }
        );

        // Now drain the master, master still open, and re-poll. Keep draining while
        // polling: the child unblocks progressively as its output is consumed.
        let mut drained = 0usize;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reaped_after_drain = None;
        while Instant::now() < deadline {
            drained += drain(master.as_raw_fd());
            if let Ok(Some(st)) = child.try_wait() {
                reaped_after_drain = st.code();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        println!(
            "    after draining {drained} bytes with the master STILL OPEN: {}",
            match reaped_after_drain {
                Some(c) => format!("REAPED code={c}"),
                None => "still not reaped".to_string(),
            }
        );

        let claim_b = reaped_without_drain.is_none() && reaped_after_drain.is_some();
        if !claim_b {
            failures.push(format!(
                "B: expected not-reaped-then-reaped-after-drain, got {reaped_without_drain:?} then {reaped_after_drain:?}"
            ));
        }
        println!(
            "    -> {}",
            if claim_b {
                "CLAIM B CONFIRMED: the blocker is undrained tty output, not the open master"
            } else if reaped_without_drain.is_some() {
                "CLAIM B REFUTED: it reaped without any drain"
            } else {
                "INCONCLUSIVE: draining did not unblock it either"
            }
        );

        // ---- Claim C: what does the master report after the child is gone? ----------
        if reaped_after_drain.is_some() || reaped_without_drain.is_some() {
            let mut rounds = Vec::new();
            for _ in 0..5 {
                let mut pfd = libc::pollfd {
                    fd: master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
                let mut buf = [0u8; 64];
                // SAFETY: master live, buf valid.
                let n = unsafe {
                    libc::read(
                        master.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                let err = if n < 0 {
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
                } else {
                    0
                };
                rounds.push(format!(
                    "rc={rc} revents={:#x} read={n} errno={err}",
                    pfd.revents
                ));
            }
            let spins = rounds.iter().filter(|r| r.contains("read=0")).count();
            let eio = rounds.iter().filter(|r| r.contains("errno=5")).count();
            if spins < 3 && eio < 3 {
                failures.push(
                    "C: end of stream did not reproduce, so the ADR-002 rule is unproven".into(),
                );
            }
            println!("[C] master after child exit, 5 rounds: {rounds:?}");
            println!(
                "    read()==0 in {spins}/5 rounds, EIO in {eio}/5 -> {}",
                if spins >= 3 || eio >= 3 {
                    "CLAIM C CONFIRMED: end of stream is not WouldBlock, so an AsyncFd loop that only handles WouldBlock spins forever"
                } else {
                    "CLAIM C REFUTED"
                }
            );
        }
        drop(master);
    }

    // ============ Claim D: canonical mode discards a paste with no newline ===========
    {
        let out_path = std::env::temp_dir().join("qoop_spike_paste.txt");
        let _ = std::fs::remove_file(&out_path);
        let (master, slave) = open_pty(80, 24)?;
        set_nonblocking(master.as_raw_fd());
        let mut child = spawn_on(
            slave.as_raw_fd(),
            "/bin/sh",
            &["-c", &format!("exec cat > {}", out_path.display())],
        )?;
        drop(slave);
        std::thread::sleep(Duration::from_millis(300));
        drain(master.as_raw_fd());

        let payload = vec![b'a'; 1 << 20];
        let mut written = 0usize;
        let mut eagain = 0usize;
        let start = Instant::now();
        while written < payload.len() && start.elapsed() < Duration::from_secs(10) {
            // SAFETY: master live, slice valid.
            let n = unsafe {
                libc::write(
                    master.as_raw_fd(),
                    payload[written..].as_ptr() as *const libc::c_void,
                    payload.len() - written,
                )
            };
            if n > 0 {
                written += n as usize;
            } else {
                eagain += 1;
                drain(master.as_raw_fd()); // keep the echo from filling the output queue
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        // Terminate the input so cat flushes and exits.
        // SAFETY: master live.
        unsafe {
            let eot = [0x04u8];
            libc::write(master.as_raw_fd(), eot.as_ptr() as *const libc::c_void, 1);
        }
        std::thread::sleep(Duration::from_millis(500));
        drain(master.as_raw_fd());
        let _ = child.kill();
        let _ = child.try_wait();
        let received = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        println!(
            "[D] 1 MiB paste with NO newline into a canonical-mode child: wrote {written} bytes \
(EAGAIN {eagain}) -> child received {received} bytes -> {}",
            if received < (written as u64) / 2 {
                "CLAIM D CONFIRMED: the line discipline discards it while write() reports success"
            } else {
                "CLAIM D REFUTED: it arrived"
            }
        );
        if received >= (written as u64) / 2 {
            failures.push(
                "D: the no-newline paste was delivered, so the ADR-002 caveat is unproven".into(),
            );
        }
        let _ = std::fs::remove_file(&out_path);
        drop(master);

        // Control: same size, newline terminated.
        let out2 = std::env::temp_dir().join("qoop_spike_paste2.txt");
        let _ = std::fs::remove_file(&out2);
        let (master2, slave2) = open_pty(80, 24)?;
        set_nonblocking(master2.as_raw_fd());
        let mut child2 = spawn_on(
            slave2.as_raw_fd(),
            "/bin/sh",
            &["-c", &format!("exec cat > {}", out2.display())],
        )?;
        drop(slave2);
        std::thread::sleep(Duration::from_millis(300));
        drain(master2.as_raw_fd());

        let mut lines = Vec::new();
        while lines.len() < (1 << 20) {
            lines.extend_from_slice(&[b'b'; 1023]);
            lines.push(b'\n');
        }
        let mut w2 = 0usize;
        let mut e2 = 0usize;
        let start = Instant::now();
        while w2 < lines.len() && start.elapsed() < Duration::from_secs(20) {
            // SAFETY: master2 live, slice valid.
            let n = unsafe {
                libc::write(
                    master2.as_raw_fd(),
                    lines[w2..].as_ptr() as *const libc::c_void,
                    lines.len() - w2,
                )
            };
            if n > 0 {
                w2 += n as usize;
            } else {
                e2 += 1;
                drain(master2.as_raw_fd());
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        // SAFETY: master2 live.
        unsafe {
            let eot = [0x04u8];
            libc::write(master2.as_raw_fd(), eot.as_ptr() as *const libc::c_void, 1);
        }
        // Wait for `cat` to flush and exit rather than sleeping a fixed interval. A fixed
        // 800 ms was measured falling short in roughly one run in ten, which would make a
        // merge gate flaky for a reason unrelated to anything under review.
        let flush_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < flush_deadline {
            drain(master2.as_raw_fd());
            if matches!(child2.try_wait(), Ok(Some(_))) {
                break;
            }
            if std::fs::metadata(&out2).map(|m| m.len()).unwrap_or(0) >= w2 as u64 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drain(master2.as_raw_fd());
        let _ = child2.kill();
        let _ = child2.try_wait();
        let got2 = std::fs::metadata(&out2).map(|m| m.len()).unwrap_or(0);
        println!(
            "    control, newline-terminated 1 KiB lines: wrote {w2} (EAGAIN {e2}) -> child received {got2} ({:.1}%)",
            if w2 > 0 {
                got2 as f64 * 100.0 / w2 as f64
            } else {
                0.0
            }
        );
        if got2 < (w2 as u64) * 9 / 10 {
            failures.push(format!(
                "D control: newline-terminated payload only delivered {got2} of {w2}, so the contrast is unproven"
            ));
        }
        println!(
            "    -> the daemon's own byte-bounded queue is not what fails; the tty line discipline is"
        );
        let _ = std::fs::remove_file(&out2);
        drop(master2);
    }

    if failures.is_empty() {
        println!("\nSPIKE-TEARDOWN: COMPLETE");
        Ok(())
    } else {
        for f in &failures {
            eprintln!("FAILURE: {f}");
        }
        std::process::exit(1);
    }
}
