//! Gates G1.8, G1.9 and G1.20: idle costs nothing.
//!
//! "Nothing changed, so nothing runs" is the rule the whole runtime is built on, and it is
//! the rule that is easiest to break invisibly: a spin shows up as a warm laptop, not as a
//! failing test. Two things make this gate real rather than decorative.
//!
//! It measures real processes, not a task inside the test binary, because the thing being
//! asserted is what the shipped daemon does.
//!
//! And it reads the CPU time from two independent operating system sources, `proc_pid_rusage`
//! and `ps`, and requires both to agree. A single source that silently returned zero would
//! make this gate pass forever, and a gate that cannot fail is worse than no gate: it is a
//! claim in the plan with nothing behind it.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Section 7: one idle terminal with a live child, 60 s. The Linux figure is larger because
/// ADR-009's Linux idle policy has an adaptive probe with a floor, where macOS gets an
/// unprivileged kqueue subscription and needs no probe at all.
#[cfg(target_os = "macos")]
const LIVE_CHILD_BUDGET: Duration = Duration::from_millis(5);
#[cfg(not(target_os = "macos"))]
const LIVE_CHILD_BUDGET: Duration = Duration::from_millis(30);
/// Section 7: one idle terminal after its child exits, 10 s. The same on either platform.
const POST_EXIT_BUDGET: Duration = Duration::from_millis(2);
/// Section 7: one idle attached client, 60 s.
const CLIENT_BUDGET: Duration = Duration::from_millis(50);

/// CPU time of a process, from the kernel's own accounting.
///
/// Deliberately not `ps`: the point of having two sources is that they are two mechanisms,
/// so a single one returning zero forever cannot make this gate pass by itself.
#[cfg(target_os = "macos")]
fn cpu_kernel(pid: u32) -> Duration {
    #[repr(C)]
    #[derive(Default)]
    struct RUsageInfoV2 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
    }
    // SAFETY: the declaration matches libproc's `proc_pid_rusage`, which takes a pid, a
    // flavor and a buffer sized for that flavor. The struct above is flavor 2's layout.
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut std::ffi::c_void) -> i32;
    }
    let mut info = RUsageInfoV2::default();
    // SAFETY: info is a valid out-param of the size flavor 2 writes.
    let rc = unsafe {
        proc_pid_rusage(
            pid as i32,
            2,
            &mut info as *mut RUsageInfoV2 as *mut std::ffi::c_void,
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage failed for pid {pid}");
    Duration::from_nanos(info.ri_user_time + info.ri_system_time)
}

/// CPU time of a process, from `/proc/<pid>/stat` fields 14 and 15, in clock ticks.
#[cfg(not(target_os = "macos"))]
fn cpu_kernel(pid: u32) -> Duration {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap_or_else(|e| panic!("cannot read /proc/{pid}/stat: {e}"));
    // The command field can contain spaces and parentheses, so fields are counted from
    // after the last close parenthesis rather than by splitting the whole line.
    let tail = &stat[stat.rfind(')').expect("a stat line") + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // After the state field, utime is index 11 and stime is index 12.
    let utime: u64 = fields[11].parse().expect("utime");
    let stime: u64 = fields[12].parse().expect("stime");
    // SAFETY: sysconf takes a name and returns a long; _SC_CLK_TCK is always defined.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    Duration::from_secs_f64((utime + stime) as f64 / ticks as f64)
}

/// CPU time of a process, from `ps`. A second observer, deliberately a different mechanism.
fn cpu_ps(pid: u32) -> Duration {
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Format is [[dd-]hh:]mm:ss, with hundredths on macOS.
    let parts: Vec<&str> = text.split(':').collect();
    let seconds: f64 = parts
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("cannot parse ps time {text:?}"));
    let minutes: f64 = if parts.len() > 1 {
        parts[parts.len() - 2].parse().unwrap_or(0.0)
    } else {
        0.0
    };
    Duration::from_secs_f64(minutes * 60.0 + seconds)
}

struct Daemon {
    child: Child,
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_daemon() -> Daemon {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let child = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .arg("daemon")
        .env("QORO_RUNTIME_DIR", dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let socket = dir.path().join("qorod.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !socket.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(socket.exists(), "the daemon never bound its socket");
    Daemon {
        child,
        _dir: dir,
        socket,
    }
}

/// Both sources, and the check that they agree.
fn measure(pid: u32, window: Duration, label: &str) -> Duration {
    let before_kernel = cpu_kernel(pid);
    let before_ps = cpu_ps(pid);
    std::thread::sleep(window);
    let used_kernel = cpu_kernel(pid) - before_kernel;
    let used_ps = cpu_ps(pid).saturating_sub(before_ps);

    // What the second source is for is catching a spin: the kernel counter says
    // microseconds and the process is actually burning a core. It is not for agreeing to
    // the microsecond, and demanding that made this flaky. `ps` reports in hundredths of a
    // second, and its TIME can include the CPU of children the process has reaped, which
    // a daemon does at every teardown: measured, 30 ms from `ps` against 0.67 ms from the
    // kernel over a 60 second window.
    //
    // So the tolerance is one percent of the window. A spin is fifty to a hundred percent
    // of it, so the thing this exists to catch is still caught by two orders of magnitude.
    let tolerance = window / 100;
    assert!(
        used_ps <= used_kernel + tolerance,
        "{label}: the two sources disagree, ps says {used_ps:?} and the kernel says {used_kernel:?}"
    );
    println!(
        "{label}: kernel {used_kernel:?}, ps {used_ps:?} over {window:?} \
         (tolerance {tolerance:?})"
    );
    used_kernel.max(used_ps.saturating_sub(tolerance))
}

#[test]
fn g1_8_an_idle_terminal_with_a_live_child_costs_almost_nothing() {
    let daemon = start_daemon();
    // A client, so the render lane is live rather than dormant.
    let mut client = attach(&daemon.socket);
    // Proves the client is really attached. If it had failed to connect it would have
    // exited, and this gate would be measuring a daemon with nobody watching.
    let _ = client_pid(client.id());
    std::thread::sleep(Duration::from_secs(2));

    let used = measure(daemon.child.id(), Duration::from_secs(60), "G1.8 daemon");
    assert!(
        used <= LIVE_CHILD_BUDGET,
        "an idle terminal with a live child used {used:?} of CPU in 60 s, budget {LIVE_CHILD_BUDGET:?}"
    );
    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn g1_9_an_idle_terminal_after_its_child_exits_costs_almost_nothing() {
    let daemon = start_daemon();
    let mut client = attach(&daemon.socket);
    let pid = client_pid(client.id());
    std::thread::sleep(Duration::from_secs(2));

    // End the CHILD, not the client. Killing the client would prove nothing here: the shell
    // is meant to survive that, which is invariant I1, so the daemon would still have a
    // live child and this would be a slower copy of the gate above.
    let shell = shell_pid(daemon.child.id());
    // SAFETY: a signal to a pid we just read from the process table.
    unsafe { libc::kill(shell as i32, libc::SIGKILL) };
    let deadline = Instant::now() + Duration::from_secs(10);
    // SAFETY: signal 0 only probes for existence.
    while Instant::now() < deadline && unsafe { libc::kill(shell as i32, 0) } == 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: signal 0 only probes for existence.
    assert_ne!(
        unsafe { libc::kill(shell as i32, 0) },
        0,
        "the child never exited, so this gate would have measured the live-child case"
    );
    std::thread::sleep(Duration::from_secs(2));

    // After this `read` returns 0 forever with POLLHUP, which under `AsyncFd` is a 100
    // percent CPU spin unless the readiness classification handles it. Measured five times
    // out of five on macOS, with no EIO.
    let used = measure(daemon.child.id(), Duration::from_secs(10), "G1.9 daemon");
    assert!(
        used <= POST_EXIT_BUDGET,
        "an idle terminal after its child exited used {used:?} of CPU in 10 s, budget {POST_EXIT_BUDGET:?}"
    );
    let _ = pid;
    let _ = client.kill();
    let _ = client.wait();
}

/// The shell the daemon started.
fn shell_pid(daemon: u32) -> u32 {
    let out = Command::new("pgrep")
        .args(["-P", &daemon.to_string()])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .expect("the daemon has a child shell")
}

#[test]
fn g1_20_an_idle_attached_client_costs_almost_nothing() {
    let daemon = start_daemon();
    let mut client = attach(&daemon.socket);
    std::thread::sleep(Duration::from_secs(2));

    // Measured on the client process, not on the daemon: a client that polls its own stdin
    // in a loop is invisible in every daemon-side measurement.
    let pid = client_pid(client.id());
    let used = measure(pid, Duration::from_secs(60), "G1.20 client");
    assert!(
        used <= CLIENT_BUDGET,
        "an idle attached client used {used:?} of CPU in 60 s, budget {CLIENT_BUDGET:?}"
    );
    let _ = client.kill();
    let _ = client.wait();
}

/// The qoro process under a `script` wrapper.
///
/// Measuring the wrapper instead of the client is a silent way to pass this gate while
/// asserting nothing: `script` sleeps in `select` and always looks idle.
fn client_pid(wrapper: u32) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let out = Command::new("pgrep")
            .args(["-P", &wrapper.to_string()])
            .output()
            .expect("pgrep");
        if let Some(pid) = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u32>().ok())
        {
            let comm = Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .expect("ps");
            let comm = String::from_utf8_lossy(&comm.stdout).trim().to_string();
            assert!(
                comm.ends_with("qoro"),
                "the child of the wrapper is {comm:?}, not the client"
            );
            return pid;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the client never appeared under its wrapper");
}

/// Attach a real client, with its stdin and stdout on a pty so it behaves as it does for a
/// person rather than taking a non-tty path.
fn attach(socket: &std::path::Path) -> Child {
    let dir = socket.parent().expect("a runtime dir");
    // `script` gives the client a controlling terminal without a terminal emulator. Without
    // one the client's raw-mode setup fails and it exits, and the gate would be measuring
    // a process that is not doing the thing.
    Command::new("script")
        .arg("-q")
        .arg("/dev/null")
        .arg(env!("CARGO_BIN_EXE_qoro"))
        .env("QORO_RUNTIME_DIR", dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the client")
}
