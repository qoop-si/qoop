//! Phase B: `qoop hook-report` against a live daemon, through the real socket.
//!
//! What this covers is the wire path: the CLI's arg parsing, the framed
//! control request, the daemon's parse-and-admit arm, and the refusal answers.
//! The positive path, a real token driving a real state change, lives one
//! layer down in `qoop-runtime`'s registry tests, because the token never
//! leaves the daemon and no socket test can honestly hold one.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self { child, dir }
    }

    fn report(&self, args: &[&str], clear_qoop_env: bool) -> (bool, String) {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.arg("hook-report");
        cmd.args(args);
        cmd.env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"));
        if clear_qoop_env {
            cmd.env_remove("QOOP_TERMINAL").env_remove("QORO_TERMINAL");
            cmd.env_remove("QOOP_TOKEN").env_remove("QORO_TOKEN");
        }
        let out = cmd.output().expect("run hook-report");
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        stderr.push_str(&String::from_utf8_lossy(&out.stdout));
        (out.status.success(), stderr)
    }

    fn qoop(&self, args: &[&str]) -> String {
        let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .output()
            .expect("run qoop");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_forged_token_is_refused_through_the_socket() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &["--state", "working", "--terminal", "1", "--token", "forged"],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

#[test]
fn an_unknown_state_is_rejected_before_anything_is_admitted() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &["--state", "busy", "--terminal", "1", "--token", "forged"],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("unknown agent state"),
        "the parse error names the state: {err:?}"
    );
}

#[test]
fn without_a_terminal_or_a_token_there_is_nothing_to_admit() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(&["--state", "working"], true);
    assert!(!ok);
    assert!(
        err.contains("no terminal") || err.contains("no token"),
        "the usage error says what is missing: {err:?}"
    );
}

#[test]
fn a_session_only_report_reaches_the_daemon_instead_of_the_usage_error() {
    // A forged token is refused by the daemon, which proves the session-only
    // path built a real request: a usage error would fail differently.
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--session-id",
            "solo-456",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

#[test]
fn neither_state_nor_session_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(&["--terminal", "1", "--token", "forged"], false);
    assert!(!ok);
    assert!(
        err.contains("usage"),
        "missing both halves names the usage: {err:?}"
    );
}

#[test]
fn an_empty_session_id_is_refused_not_recorded_as_silence() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--terminal",
            "1",
            "--token",
            "forged",
            "--session-id",
            "   ",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("usage"),
        "an empty id is a caller error, not a report: {err:?}"
    );
}

#[test]
fn an_unknown_event_is_rejected_before_anything_is_admitted() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--event",
            "started",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("unknown focus event"),
        "the parse error names the event: {err:?}"
    );
}

#[test]
fn a_label_without_an_event_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--label",
            "Read foo.rs",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("--label needs --event"),
        "a label alone names the usage: {err:?}"
    );
}

#[test]
fn an_event_on_the_session_only_path_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--session-id",
            "solo-456",
            "--event",
            "tool",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("--event/--label need --state"),
        "an event without a state names the usage: {err:?}"
    );
}

#[test]
fn an_event_report_reaches_the_daemon_instead_of_the_usage_error() {
    // A forged token is refused by the daemon, which proves the event path
    // built a real request: a usage error would fail differently.
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--event",
            "tool",
            "--label",
            "Read foo.rs",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

/// A daemon that accepts the connection and never says hello is still bounded by the
/// documented five seconds. Every agent turn waits on this command, so a stuck daemon
/// must cost a hook five seconds and not the agent's own hook timeout.
#[test]
fn a_daemon_that_never_greets_still_ends_the_report_in_five_seconds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = qoop_testkit::socket_in(dir.path());
    let mut private = dir.path().to_path_buf();
    for part in [None, Some("sessions"), Some("default")] {
        if let Some(part) = part {
            private.push(part);
            std::fs::create_dir(&private).expect("mkdir");
        }
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    // Bound and listening, so the connect lands in the backlog, and never accepted.
    let _wedged = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let started = Instant::now();
    let mut child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args([
            "hook-report",
            "--state",
            "working",
            "--terminal",
            "1",
            "--token",
            "t",
        ])
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env_remove("QOOP_SESSION")
        .env_remove("QORO_SESSION")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("run hook-report");
    let deadline = started + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let status = status.expect("hook-report hung on a daemon that never answered");
    assert!(!status.success(), "no answer is not a report");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "took {:?}",
        started.elapsed()
    );
}

/// A `starting` report can land after a program drew its composer and went quiet: the
/// screen is only read after output, so without a read asked for by the report the row
/// stayed `starting` (a working animation in the sidebar) until the first prompt.
#[test]
fn a_starting_report_after_the_composer_is_drawn_settles_to_idle() {
    let daemon = Daemon::start();
    let dir = daemon.dir.path();
    let reported = dir.join("reported");
    let rule = "\u{2500}".repeat(40);
    let agent = dir.join("claude");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf '{rule}\\n{prompt} Try \"fix it\"\\n{rule}\\n  ? for shortcuts\\n'\n\
             sleep 1\n'{qoop}' hook-report --state starting\ntouch '{reported}'\nsleep 60\n",
            prompt = "\u{276f}",
            qoop = env!("CARGO_BIN_EXE_qoop"),
            reported = reported.display(),
        ),
    )
    .expect("write");
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let number = daemon
        .qoop(&["new", "--", agent.to_str().unwrap()])
        .trim()
        .to_string();
    let line = |daemon: &Daemon| {
        daemon
            .qoop(&["agents"])
            .lines()
            .find(|l| l.starts_with(&format!("term={number} ")))
            .unwrap_or_default()
            .to_string()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !reported.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(reported.exists(), "the stand in never reported");
    while Instant::now() < deadline && !line(&daemon).contains("state=idle") {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        line(&daemon).contains("state=idle"),
        "a composer on screen must end the starting claim: {}",
        line(&daemon)
    );
}

#[test]
fn a_sub_agent_report_reaches_the_daemon_and_mixing_it_with_a_state_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--subagent-start",
            "a1",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--subagents",
            "a1",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(err.contains("stand alone"), "{err:?}");
}

/// What an agent's hooks say of its sub-agents reaches its row as a count, and its state is
/// left alone.
#[test]
fn sub_agents_a_hook_names_are_counted_on_the_agents_row() {
    let daemon = Daemon::start();
    let dir = daemon.dir.path();
    let reported = dir.join("reported");
    let rule = "\u{2500}".repeat(40);
    let agent = dir.join("claude");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf '{rule}\\n{prompt} Try \"fix it\"\\n{rule}\\n  ? for shortcuts\\n'\n\
             sleep 1\nfor f in '--subagent-start a1' '--subagent-start a2' '--subagent-start a3' \
             '--subagent-end a1'; do '{qoop}' hook-report $f; done\ntouch '{reported}'\nsleep 60\n",
            prompt = "\u{276f}",
            qoop = env!("CARGO_BIN_EXE_qoop"),
            reported = reported.display(),
        ),
    )
    .expect("write");
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let number = daemon
        .qoop(&["new", "--", agent.to_str().unwrap()])
        .trim()
        .to_string();
    let line = |daemon: &Daemon| {
        daemon
            .qoop(&["agents"])
            .lines()
            .find(|l| l.starts_with(&format!("term={number} ")))
            .unwrap_or_default()
            .to_string()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !reported.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(reported.exists(), "the stand in never reported");
    while Instant::now() < deadline && !line(&daemon).contains("subagents=2") {
        std::thread::sleep(Duration::from_millis(100));
    }
    let row = line(&daemon);
    assert!(row.contains(" background=2 subagents=2"), "{row}");
    assert!(
        row.contains("state=idle"),
        "the state is the screen's: {row}"
    );
}

/// An agent the terminal's agent started (Claude running `codex exec`) inherits the terminal and
/// its token: a report that names it is refused, one that names the terminal's own is counted.
#[test]
fn a_report_from_an_agent_the_terminals_agent_started_is_refused() {
    let daemon = Daemon::start();
    let dir = daemon.dir.path();
    let reported = dir.join("reported");
    let rule = "\u{2500}".repeat(40);
    let agent = dir.join("claude");
    std::fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf '{rule}\\n{prompt} Try \"fix it\"\\n{rule}\\n  ? for shortcuts\\n'\n\
             sleep 1\n'{qoop}' hook-report --agent codex --subagent-start c1 && touch '{foreign}'\n\
             '{qoop}' hook-report --agent claude --subagent-start a1\ntouch '{reported}'\nsleep 60\n",
            prompt = "\u{276f}",
            qoop = env!("CARGO_BIN_EXE_qoop"),
            foreign = dir.join("foreign-admitted").display(),
            reported = reported.display(),
        ),
    )
    .expect("write");
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let number = daemon
        .qoop(&["new", "--", agent.to_str().unwrap()])
        .trim()
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !reported.exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(reported.exists(), "the stand in never reported");
    assert!(
        !dir.join("foreign-admitted").exists(),
        "codex's report was admitted"
    );
    let row = daemon
        .qoop(&["agents"])
        .lines()
        .find(|l| l.starts_with(&format!("term={number} ")))
        .unwrap_or_default()
        .to_string();
    assert!(
        row.contains(" subagents=1"),
        "only claude's own is counted: {row}"
    );
}

/// opencode has no hook here: the count in its footer is read off its screen into its row.
#[test]
fn opencodes_footer_count_reaches_its_row() {
    let daemon = Daemon::start();
    let dir = daemon.dir.path();
    let agent = dir.join("opencode");
    std::fs::write(
        &agent,
        "#!/bin/sh\nprintf '  Build \\302\\267 a model\\n   esc interrupt     \\342\\206\\223 3 subagents  ctrl+p commands'\nsleep 60\n",
    )
    .expect("write");
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let number = daemon
        .qoop(&["new", "--", agent.to_str().unwrap()])
        .trim()
        .to_string();
    let line = |daemon: &Daemon| {
        daemon
            .qoop(&["agents"])
            .lines()
            .find(|l| l.starts_with(&format!("term={number} ")))
            .unwrap_or_default()
            .to_string()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !line(&daemon).contains("subagents=3") {
        std::thread::sleep(Duration::from_millis(100));
    }
    let row = line(&daemon);
    assert!(row.contains("program=opencode"), "{row}");
    assert!(row.contains(" background=3 subagents=3"), "{row}");
}

/// A turn's closing words ride the report as `--said`: parsed and sent, never a usage error
/// (an older qoop refusing it is what the Claude hook's resend is for).
#[test]
fn the_closing_words_are_a_flag_the_report_takes() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "idle",
            "--event",
            "done",
            "--label",
            "Merge now?",
            "--said",
            "Built it.\n\nMerge now?",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forged token is still refused");
    assert!(!err.contains("unknown flag"), "{err}");
    assert!(err.contains("refused"), "it reached the daemon: {err}");
}
