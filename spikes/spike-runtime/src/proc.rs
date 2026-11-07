//! Spike: can a silent foreground transition be observed without polling, and does the
//! idle path really cost zero CPU when measured by the OS rather than by our own counter?
//!
//! This exists because a second look correctly rejected two claims in ADR-009
//! and ADR-002:
//!   a) "a foreground process group change cannot happen without output or SIGCHLD" is
//!      false. `exec` in place and a silently started job both change the foreground
//!      process with no bytes on the PTY.
//!   b) the previous spike's "wakeup counter" was incremented only inside a read helper
//!      that was not running during the idle measurement, so it proved nothing.
//!
//! Checks:
//!   1. reproduce the silent transition and show that output-only detection misses it
//!   2. kqueue EVFILT_PROC / NOTE_EXEC as a real, unprivileged, edge-triggered source
//!   3. measure idle cost with getrusage, which the OS computes, not with our own counter

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn rusage_cpu() -> Duration {
    // SAFETY: out-param is a fully owned, correctly sized rusage.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: RUSAGE_SELF with a valid out-param.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(rc, 0, "getrusage failed");
    let u = Duration::new(ru.ru_utime.tv_sec as u64, ru.ru_utime.tv_usec as u32 * 1000);
    let s = Duration::new(ru.ru_stime.tv_sec as u64, ru.ru_stime.tv_usec as u32 * 1000);
    u + s
}

struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fd is a live descriptor for the duration of the call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn is_cloexec(fd: RawFd) -> bool {
    // SAFETY: fd is a live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0 && (flags & libc::FD_CLOEXEC) != 0
}

fn open_pty(cols: u16, rows: u16) -> std::io::Result<Pty> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty writes two valid fds on success; ws is fully initialised.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut ws,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both fds were just produced by openpty and are owned by nobody else.
    unsafe {
        Ok(Pty {
            master: OwnedFd::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        })
    }
}

fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fd is live.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn foreground_pgrp(fd: RawFd) -> Option<libc::pid_t> {
    // SAFETY: fd is a live PTY master.
    let pgrp = unsafe { libc::tcgetpgrp(fd) };
    if pgrp < 0 { None } else { Some(pgrp) }
}

fn dup_for_child(fd: RawFd) -> std::io::Result<Stdio> {
    // SAFETY: fd is live; dup returns a fresh descriptor owned by nobody else.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: dup is fresh and unowned.
    Ok(unsafe { Stdio::from(OwnedFd::from_raw_fd(dup)) })
}

fn spawn_shell(pty: &Pty) -> std::io::Result<Child> {
    let slave_fd = pty.slave.as_raw_fd();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-i");
    // SAFETY: only async-signal-safe syscalls run post-fork, with no allocation.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.stdin(dup_for_child(slave_fd)?)
            .stdout(dup_for_child(slave_fd)?)
            .stderr(dup_for_child(slave_fd)?)
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

fn read_all_available(fd: RawFd) -> usize {
    let mut buf = [0u8; 65536];
    let mut total = 0usize;
    loop {
        // SAFETY: fd is live and buf is a valid writable slice.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    total
}

/// Count how many descriptors a process holds, using the OS as ground truth.
#[cfg(target_os = "macos")]
fn child_pty_devices(pid: i32) -> Option<Vec<String>> {
    let out = Command::new("/usr/sbin/lsof")
        .args(["-p", &pid.to_string(), "-Fn"])
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with('n'))
            .map(|l| l[1..].to_string())
            .filter(|n| n.contains("/dev/tty") || n.contains("/dev/pty") || n.contains("ptmx"))
            .collect(),
    )
}
#[cfg(not(target_os = "macos"))]
fn child_pty_devices(pid: i32) -> Option<Vec<String>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(format!("/proc/{pid}/fd")).ok()? {
        if let Ok(e) = e {
            if let Ok(t) = std::fs::read_link(e.path()) {
                let t = t.to_string_lossy().to_string();
                if t.contains("/dev/pts") || t.contains("ptmx") {
                    v.push(t);
                }
            }
        }
    }
    Some(v)
}

/// kqueue EVFILT_PROC: does the kernel tell us about exec without any polling and
/// without elevated privileges?
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn watch_proc_exec(pid: i32, wait: Duration) -> std::io::Result<(bool, bool)> {
    // SAFETY: kqueue takes no arguments and returns a descriptor or -1.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: kq is a live descriptor we own for the rest of this function.
    let kq = unsafe { OwnedFd::from_raw_fd(kq) };

    let changes = [libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
        fflags: libc::NOTE_EXEC | libc::NOTE_EXIT | libc::NOTE_FORK,
        data: 0,
        udata: std::ptr::null_mut(),
    }];
    // SAFETY: zeroed kevent is a valid out-param for kevent().
    let mut events: [libc::kevent; 4] = unsafe { std::mem::zeroed() };
    let ts = libc::timespec {
        tv_sec: wait.as_secs() as _,
        tv_nsec: wait.subsec_nanos() as _,
    };
    // SAFETY: kq is live, changes and events are correctly sized arrays, ts is valid.
    let n = unsafe {
        libc::kevent(
            kq.as_raw_fd(),
            changes.as_ptr(),
            changes.len() as _,
            events.as_mut_ptr(),
            events.len() as _,
            &ts,
        )
    };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut saw_exec = false;
    let mut saw_fork = false;
    for e in events.iter().take(n as usize) {
        if e.fflags & libc::NOTE_EXEC != 0 {
            saw_exec = true;
        }
        if e.fflags & libc::NOTE_FORK != 0 {
            saw_fork = true;
        }
    }
    Ok((saw_exec, saw_fork))
}
#[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
fn watch_proc_exec(_pid: i32, _wait: Duration) -> std::io::Result<(bool, bool)> {
    Ok((false, false))
}

/// Bounded, non-blocking reap that keeps draining the master.
///
/// spike-teardown claim B showed why the drain matters: an exiting child is not reapable
/// until its pending tty output has been read, and closing the master only appears to fix
/// that because closing discards the output. So teardown drains, it does not close.
fn reap_bounded(child: &mut Child, master: Option<RawFd>, budget: Duration) -> Option<i32> {
    let deadline = Instant::now() + budget;
    let _ = child.kill();
    while Instant::now() < deadline {
        if let Some(fd) = master {
            let _ = read_all_available(fd);
        }
        match child.try_wait() {
            Ok(Some(status)) => return status.code().or(Some(-1)),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut failures: Vec<String> = Vec::new();

    // ---- 0. baseline: are openpty descriptors close-on-exec by default? -----------
    let probe = open_pty(80, 24)?;
    let master_cloexec = is_cloexec(probe.master.as_raw_fd());
    let slave_cloexec = is_cloexec(probe.slave.as_raw_fd());
    println!(
        "[0] openpty default FD_CLOEXEC: master={master_cloexec}, slave={slave_cloexec} \
         (both false means every spawn leaks every other terminal's fds unless we set it)"
    );
    drop(probe);

    // ---- 1. fd leakage into the child, measured against the real fd table ---------
    // Terminal X exists first, then terminal Y spawns a child. Does Y's child inherit
    // X's descriptors? Run it both ways so the result is a proof, not an anecdote.
    fn leak_probe(protect_other: bool) -> std::io::Result<(Vec<String>, Child, Pty)> {
        let other = open_pty(80, 24)?;
        if protect_other {
            set_cloexec(other.master.as_raw_fd())?;
            set_cloexec(other.slave.as_raw_fd())?;
        }
        let own = open_pty(80, 24)?;
        set_cloexec(own.master.as_raw_fd())?;
        set_cloexec(own.slave.as_raw_fd())?;
        set_nonblocking(own.master.as_raw_fd())?;
        let child = spawn_shell(&own)?;
        std::thread::sleep(Duration::from_millis(300));
        let mut devices = child_pty_devices(child.id() as i32).unwrap_or_default();
        devices.sort();
        devices.dedup();
        drop(other);
        Ok((devices, child, own))
    }

    let (leaky_devices, mut leaky_child, leaky_pty) = leak_probe(false)?;
    let (safe_devices, mut safe_child, safe_pty) = leak_probe(true)?;
    // The child legitimately holds its own slave on fds 0, 1 and 2, which lsof reports
    // as one device. More than one distinct device means it inherited another terminal.
    let leak_demonstrated = leaky_devices.len() > 1;
    let cloexec_fixes_it = safe_devices.len() <= 1;
    println!(
        "[1] fd inheritance: without CLOEXEC the child sees {:?}; with CLOEXEC it sees {:?} -> {}",
        leaky_devices,
        safe_devices,
        if leak_demonstrated && cloexec_fixes_it {
            "PASS"
        } else {
            "FAIL"
        }
    );
    if !leak_demonstrated {
        failures
            .push("could not demonstrate the fd leak, so the probe is not proving anything".into());
    }
    if !cloexec_fixes_it {
        failures.push(format!("CLOEXEC did not stop the leak: {safe_devices:?}"));
    }
    let _ = reap_bounded(
        &mut leaky_child,
        Some(leaky_pty.master.as_raw_fd()),
        Duration::from_secs(3),
    );
    let _ = reap_bounded(
        &mut safe_child,
        Some(safe_pty.master.as_raw_fd()),
        Duration::from_secs(3),
    );

    // Terminal used by the rest of the spike.
    let b = open_pty(80, 24)?;
    set_cloexec(b.master.as_raw_fd())?;
    set_cloexec(b.slave.as_raw_fd())?;
    set_nonblocking(b.master.as_raw_fd())?;
    let mut child = spawn_shell(&b)?;
    drop(b.slave);

    // ---- 2. the silent transition a second look said we would miss ----------------
    let mfd = b.master.as_raw_fd();
    let mut w = unsafe { std::fs::File::from_raw_fd(libc::dup(mfd)) };
    // let the shell settle and drain its prompt
    std::thread::sleep(Duration::from_millis(400));
    read_all_available(mfd);
    let pgrp_before = foreground_pgrp(mfd);

    // `exec` replaces the shell process image in place. No fork, so no SIGCHLD for a
    // new child, and `sleep` writes nothing at all to the terminal.
    w.write_all(b"exec sleep 4\n")?;
    w.flush()?;
    std::thread::sleep(Duration::from_millis(400));
    let bytes_after = read_all_available(mfd);
    let pgrp_after = foreground_pgrp(mfd);
    println!(
        "[2] silent exec transition: pgrp {:?} -> {:?}, bytes emitted after the command line: {}",
        pgrp_before, pgrp_after, bytes_after
    );
    println!("    note: pgrp is unchanged by exec, so a tcgetpgrp comparison alone cannot see it");

    // ---- 3. kqueue EVFILT_PROC as an unprivileged, edge-triggered source ----------
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        // Register on a fresh child and make it exec, then see if the kernel reports it.
        let c = open_pty(80, 24)?;
        set_cloexec(c.master.as_raw_fd())?;
        set_cloexec(c.slave.as_raw_fd())?;
        set_nonblocking(c.master.as_raw_fd())?;
        let mut child2 = spawn_shell(&c)?;
        let pid2 = child2.id() as i32;
        drop(c.slave);
        std::thread::sleep(Duration::from_millis(300));
        read_all_available(c.master.as_raw_fd());

        let watcher = std::thread::spawn(move || watch_proc_exec(pid2, Duration::from_secs(3)));
        std::thread::sleep(Duration::from_millis(150));
        // SAFETY: dup of a live master fd, owned by this File.
        let mut w2 = unsafe { std::fs::File::from_raw_fd(libc::dup(c.master.as_raw_fd())) };
        w2.write_all(b"exec sleep 2\n").ok();
        w2.flush().ok();

        let (saw_exec, saw_fork) = watcher.join().unwrap()?;
        println!(
            "[3] kqueue EVFILT_PROC on an unprivileged child: exec={} fork={} -> {}",
            saw_exec,
            saw_fork,
            if saw_exec { "PASS" } else { "FAIL" }
        );
        if !saw_exec {
            failures.push("kqueue NOTE_EXEC did not fire for an in-place exec".into());
        }
        drop(w2);
        let reaped = reap_bounded(
            &mut child2,
            Some(c.master.as_raw_fd()),
            Duration::from_secs(6),
        );
        if reaped.is_none() {
            failures.push("step 3 child did not reap within 6s while draining the master".into());
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    {
        println!("[3] kqueue EVFILT_PROC: SKIPPED (not a kqueue platform)");
    }

    // ---- 4. idle cost measured by the OS, not by our own counter ------------------
    // A fresh terminal whose child is alive but silent. This is the shape of a real
    // idle workspace: a shell sitting at a prompt with nothing happening.
    let idle_pty = open_pty(80, 24)?;
    set_cloexec(idle_pty.master.as_raw_fd())?;
    set_cloexec(idle_pty.slave.as_raw_fd())?;
    set_nonblocking(idle_pty.master.as_raw_fd())?;
    let mut idle_child = spawn_shell(&idle_pty)?;
    let ifd = idle_pty.master.as_raw_fd();
    drop(idle_pty.slave);
    std::thread::sleep(Duration::from_millis(500));
    let drained = read_all_available(ifd);

    let cpu_before = rusage_cpu();
    let wall_start = Instant::now();
    let mut pfd = libc::pollfd {
        fd: ifd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd points at one valid pollfd for the duration of the call.
    let rc = unsafe { libc::poll(&mut pfd, 1, 3000) };
    let wall = wall_start.elapsed();
    let cpu_used = rusage_cpu().saturating_sub(cpu_before);
    let idle_ok = rc == 0 && wall >= Duration::from_secs(3) && cpu_used < Duration::from_millis(5);
    println!(
        "[4] OS-measured idle cost over {:?}: {:?} of CPU (poll rc={}, revents={:#x}, drained {} prompt bytes) -> {}",
        wall,
        cpu_used,
        rc,
        pfd.revents,
        drained,
        if idle_ok { "PASS" } else { "FAIL" }
    );
    if !idle_ok {
        failures.push(format!(
            "idle window consumed {cpu_used:?} cpu with poll rc={rc} revents={:#x}",
            pfd.revents
        ));
    }
    if reap_bounded(&mut idle_child, Some(ifd), Duration::from_secs(2)).is_none() {
        failures.push("idle child did not reap within 2s".into());
    }

    if reap_bounded(&mut child, Some(mfd), Duration::from_secs(2)).is_none() {
        failures.push("main child did not reap within 2s".into());
    }

    if failures.is_empty() {
        println!("\nSPIKE-PROC: ALL PASS");
        Ok(())
    } else {
        for f in &failures {
            eprintln!("FAILURE: {f}");
        }
        std::process::exit(1);
    }
}
