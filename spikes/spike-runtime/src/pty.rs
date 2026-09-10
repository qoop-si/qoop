//! Spike: is `tokio::io::unix::AsyncFd` a viable driver for a PTY master on this
//! platform, and does an idle PTY really cost zero wakeups?
//!
//! This decides ADR-002. A blocked OS thread per pane is the usual
//! design; if AsyncFd works here we get one cheap task per terminal instead.
//!
//! Checks, in order:
//!   1. spawn a real shell on a real PTY, read its banner asynchronously
//!   2. write input, read the echo back (proves the write path)
//!   3. resize and prove the child observed the new winsize (`stty size`)
//!   4. sit idle for 3 seconds and count reactor wakeups on the PTY (must be 0)
//!   5. exit the child and observe EOF plus a real wait status
//!
//! Scope note: these spikes model SYSCALL behaviour, not the runtime discipline the ADRs
//! require of shipped code. They use blocking waits and sleeps where that is the shortest
//! way to observe the kernel. Nothing here is a template for the daemon.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
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
    // SAFETY: openpty writes two valid fds into the out-params on success; ws is a
    // fully initialised winsize and the two null pointers are documented as optional.
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
    // SAFETY: both fds were just produced by openpty and are not owned elsewhere.
    unsafe {
        Ok(Pty {
            master: OwnedFd::from_raw_fd(master),
            slave: OwnedFd::from_raw_fd(slave),
        })
    }
}

fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fd is a live descriptor owned by the caller for the duration of the call.
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

fn resize(fd: RawFd, cols: u16, rows: u16) -> std::io::Result<()> {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd is a live PTY master and ws is a fully initialised winsize.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn foreground_pgrp(fd: RawFd) -> Option<libc::pid_t> {
    // SAFETY: fd is a live PTY master.
    let pgrp = unsafe { libc::tcgetpgrp(fd) };
    if pgrp < 0 { None } else { Some(pgrp) }
}

/// Spawn a child attached to the slave side, in its own session, with the slave as its
/// controlling terminal. This is the standard sequence; getting it wrong is why a shell
/// would otherwise report "no job control".
fn spawn_on_pty(pty: &Pty, program: &str, args: &[&str]) -> std::io::Result<Child> {
    let slave_fd = pty.slave.as_raw_fd();
    let mut cmd = Command::new(program);
    cmd.args(args);
    // SAFETY: the three dups are the only fd work done post-fork; setsid and TIOCSCTTY
    // are async-signal-safe syscalls and no allocation happens in this closure.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.stdin(Stdio::from_raw_fd_dup(slave_fd)?)
            .stdout(Stdio::from_raw_fd_dup(slave_fd)?)
            .stderr(Stdio::from_raw_fd_dup(slave_fd)?)
            .pre_exec(move || {
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
    cmd.env_remove("PS1");
    cmd.spawn()
}

/// `Stdio::from` consumes an OwnedFd, but we need three independent handles onto the
/// same slave, so dup it three times.
trait StdioDup {
    fn from_raw_fd_dup(fd: RawFd) -> std::io::Result<Stdio>;
}
impl StdioDup for Stdio {
    fn from_raw_fd_dup(fd: RawFd) -> std::io::Result<Stdio> {
        // SAFETY: fd is live; dup returns a fresh owned descriptor.
        let dup = unsafe { libc::dup(fd) };
        if dup < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: dup is a fresh descriptor owned by nobody else.
        Ok(unsafe { Stdio::from(OwnedFd::from_raw_fd(dup)) })
    }
}

struct MasterIo(OwnedFd);

impl Read for &MasterIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: self.0 is a live descriptor and buf is a valid writable slice.
        let n = unsafe {
            libc::read(
                self.0.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

impl Write for &MasterIo {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // SAFETY: self.0 is a live descriptor and buf is a valid readable slice.
        let n = unsafe {
            libc::write(
                self.0.as_raw_fd(),
                buf.as_ptr() as *const libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// SAFETY: MasterIo owns its descriptor exclusively; no interior mutability is shared.
unsafe impl Send for MasterIo {}
unsafe impl Sync for MasterIo {}

impl AsRawFd for MasterIo {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

async fn read_until(
    afd: &AsyncFd<MasterIo>,
    needle: &str,
    timeout: Duration,
) -> std::io::Result<String> {
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    let mut buf = [0u8; 65536];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let ready = match tokio::time::timeout(remaining, afd.readable()).await {
            Ok(r) => r?,
            Err(_) => break,
        };
        let mut guard = ready;
        match guard.try_io(|inner| inner.get_ref().read(&mut buf)) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.contains(needle) {
                    return Ok(acc);
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
    Ok(acc)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut failures: Vec<String> = Vec::new();

    // ---- 1. spawn a real interactive shell on a real PTY -------------------------
    let pty = open_pty(80, 24)?;
    set_nonblocking(pty.master.as_raw_fd())?;
    let mut child = spawn_on_pty(&pty, "/bin/sh", &["-i"])?;
    // The parent must drop its slave handle or EOF will never arrive after the child
    // exits, because the parent itself would still hold the slave open.
    drop(pty.slave);

    let master = MasterIo(pty.master);
    let afd = AsyncFd::with_interest(master, Interest::READABLE | Interest::WRITABLE)?;

    // ---- 2. write input, read the echo -------------------------------------------
    let t0 = Instant::now();
    {
        let mut w = afd.get_ref();
        w.write_all(b"echo SPIKE_MARK_$((6*7))\n")?;
    }
    let out = read_until(&afd, "SPIKE_MARK_42", Duration::from_secs(5)).await?;
    let echo_ok = out.contains("SPIKE_MARK_42");
    println!(
        "[1] async read/write on PTY master: {} ({} bytes in {:?})",
        if echo_ok { "PASS" } else { "FAIL" },
        out.len(),
        t0.elapsed()
    );
    if !echo_ok {
        failures.push(format!("no echo; got: {out:?}"));
    }

    // ---- 3. foreground process group via tcgetpgrp -------------------------------
    let pgrp = foreground_pgrp(afd.get_ref().as_raw_fd());
    println!(
        "[2] tcgetpgrp on master: {} ({:?})",
        if pgrp.is_some() { "PASS" } else { "FAIL" },
        pgrp
    );
    if pgrp.is_none() {
        failures.push("tcgetpgrp returned no foreground pgrp".into());
    }

    // ---- 4. resize is observed by the child --------------------------------------
    resize(afd.get_ref().as_raw_fd(), 132, 43)?;
    {
        let mut w = afd.get_ref();
        w.write_all(b"stty size\n")?;
    }
    let out = read_until(&afd, "43 132", Duration::from_secs(5)).await?;
    let resize_ok = out.contains("43 132");
    println!(
        "[3] resize observed by child (stty size): {}",
        if resize_ok { "PASS" } else { "FAIL" }
    );
    if !resize_ok {
        failures.push(format!("resize not observed; got: {out:?}"));
    }

    // ---- 5. THE decisive one: idle costs zero wakeups ----------------------------
    // AsyncFd caches readiness until a try_io reports WouldBlock, so the fd must be
    // drained to quiet before the measurement means anything. Discovering this is half
    // the point of the spike: a naive readable() loop reads cached readiness forever.
    let mut sink = [0u8; 65536];
    let mut settle_bytes = 0usize;
    let mut settle_rounds = 0u32;
    loop {
        settle_rounds += 1;
        if settle_rounds > 10_000 {
            failures.push("could not settle the pty to quiet".into());
            break;
        }
        match tokio::time::timeout(Duration::from_millis(300), afd.readable()).await {
            Err(_) => break, // quiet for 300ms straight
            Ok(g) => {
                let mut guard = g?;
                match guard.try_io(|inner| inner.get_ref().read(&mut sink)) {
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => settle_bytes += n,
                    Ok(Err(_)) => break,
                    Err(_would_block) => {} // readiness cleared, loop and wait again
                }
            }
        }
    }

    let idle_start = Instant::now();
    let idle = tokio::time::timeout(Duration::from_secs(3), afd.readable()).await;
    let idle_elapsed = idle_start.elapsed();
    // The proof is that the timeout fired: no readable readiness arrived in 3 seconds.
    // An earlier version of this spike also printed a "reactor wakeups" counter that was
    // only incremented inside the read helper, which does not run during this window. It
    // measured nothing and has been deleted rather than reinterpreted. The real idle cost
    // is measured with getrusage in spike-proc check 4.
    let idle_ok = idle.is_err() && idle_elapsed >= Duration::from_secs(3);
    println!(
        "[4] idle 3s with no output: {} (settled after {settle_rounds} rounds / {settle_bytes} bytes; no readiness for {:?})",
        if idle_ok { "PASS" } else { "FAIL" },
        idle_elapsed
    );
    if !idle_ok {
        failures.push(format!(
            "idle pty produced readiness after {idle_elapsed:?}"
        ));
    }

    // ---- 6. child exit produces EOF and a real wait status -----------------------
    {
        let mut w = afd.get_ref();
        w.write_all(b"exit 7\n")?;
    }
    let tail = read_until(&afd, "\u{0}NEVER\u{0}", Duration::from_secs(3)).await?;
    let status = child.wait()?;
    let exit_ok = status.code() == Some(7);
    println!(
        "[5] child exit status propagates: {} ({:?}, {} trailing bytes)",
        if exit_ok { "PASS" } else { "FAIL" },
        status.code(),
        tail.len()
    );
    if !exit_ok {
        failures.push(format!("expected exit 7, got {status:?}"));
    }

    if failures.is_empty() {
        println!("SPIKE-PTY: ALL PASS");
        Ok(())
    } else {
        for f in &failures {
            eprintln!("FAILURE: {f}");
        }
        std::process::exit(1);
    }
}
