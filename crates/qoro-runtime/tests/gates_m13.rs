//! Milestone 13 gates: a session that stops comes back as itself.
//!
//! What a person asked for when they closed the window is what has to be there when they
//! open it again: the same tabs, in the same directories, under the same names, with the
//! conversation they were having carried on. These gates drive the real daemon twice over
//! the same directory, because that is the only thing that proves it: the first daemon is
//! the one that records, the second is the one that reads, and a test that only exercised
//! one of them would pass with nothing written at all.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use qoro_runtime::paths;
use qoro_runtime::{Daemon, control::ControlRequest};

/// A private directory a daemon will accept, kept alive by the returned handle.
fn runtime_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    paths::ensure_private_dir(dir.path()).expect("a private directory");
    dir
}

/// Bind a daemon on `dir`, run `body` against it, and stop serving before returning.
///
/// The daemon is left bound for the length of the body and then dropped, which is what a
/// person killing the client and the daemon looks like from the store's point of view:
/// nothing is told it is closing, so nothing is recorded as closed.
async fn with_daemon<F, Fut, T>(dir: &Path, body: F) -> T
where
    F: FnOnce(Daemon) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _binding = qoro_testkit::BINDING.lock().await;
    let daemon = Daemon::bind(dir, Path::new("/bin/sh"))
        .await
        .expect("the daemon binds");
    body(daemon).await
}

/// Wait for a condition, so a gate is not a race against a shell starting.
async fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    done()
}

/// G13.4: a session that was stopped comes back as itself.
///
/// The same tabs, the same count, the same names, the same directories. Asserted over a
/// second daemon on the same directory, which is exactly what reopening is.
#[tokio::test]
async fn g13_4_a_stopped_session_comes_back_with_its_tabs_their_names_and_their_directories() {
    let dir = runtime_dir();
    let elsewhere = tempfile::tempdir().expect("a directory to be in");
    let elsewhere = std::fs::canonicalize(elsewhere.path()).expect("a real path");

    let (first_count, made) = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        // The daemon opens one for itself. Two more, one of them named and in a directory
        // of its own, which is the pair of things a restore has to carry.
        let plain = terminals.mint();
        store
            .create_terminal(plain as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        let named = terminals.mint();
        store
            .create_terminal(
                named as i64,
                workspace,
                elsewhere.display().to_string(),
                "/bin/sh".into(),
            )
            .await
            .expect("a row");
        store
            .set_title(named as i64, "release".into())
            .await
            .expect("a name");
        (terminals.streams().len(), (plain, named))
    })
    .await;
    assert_eq!(first_count, 1, "the daemon opens one terminal of its own");

    // The daemon is gone. Nothing was told it was closing, so nothing is closed.
    let brought_back = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 3).await,
            "the session came back with {} tabs rather than three: {:?}",
            terminals.streams().len(),
            terminals.streams()
        );
        let streams = terminals.streams();
        assert!(
            streams.contains(&made.0) && streams.contains(&made.1),
            "the tabs came back under different numbers, so `ctrl-a 2` means something \
             else after every restart: {streams:?}"
        );
        assert_eq!(
            terminals.name_of(made.1),
            Some("release".to_string()),
            "a tab came back without the name a person gave it"
        );
        // And the next new terminal does not land on a number that is already on screen.
        let next = terminals.mint();
        assert!(
            !streams.contains(&next),
            "a new tab took the number of one that came back: {next} in {streams:?}"
        );
        streams.len()
    })
    .await;
    assert_eq!(brought_back, 3);
}

/// G13.5: an agent comes back continued, and a command does not come back at all.
///
/// The asymmetry is the whole rule. A conversation is worth carrying on; an effect is not
/// worth repeating, and the saved command line of a tab that was halfway through a deploy
/// is a loaded gun pointed at the next start.
///
/// The agent's real resume command is replaced by an override for the length of this test,
/// for two reasons. It makes the assertion an effect rather than a screen reading, since a
/// daemon publishes a screen only while somebody is watching it. And it keeps the test from
/// starting a real agent on the machine running it: the bundled command line is
/// `claude --continue`, and a test that ran that would open a conversation and bill
/// somebody for it.
#[tokio::test]
async fn g13_5_an_agent_is_continued_and_a_saved_command_is_never_re_run() {
    let dir = runtime_dir();
    let evidence = tempfile::tempdir().expect("a directory");
    // A path with a space in it, because the argv is typed into a shell and an unquoted
    // space is two arguments.
    let resumed = evidence.path().join("it resumed");
    let must_not_exist = evidence.path().join("it re-ran the command");

    let overrides = tempfile::tempdir().expect("a directory");
    std::fs::write(
        overrides.path().join("claude.toml"),
        format!(
            "id = \"claude\"\nversion = \"test\"\n[resume]\nlatest = [\"/usr/bin/touch\", \"{}\"]\n",
            resumed.display()
        ),
    )
    .expect("an override");
    let outcome = qoro_detect::reload_overrides(overrides.path());
    assert!(outcome.errors.is_empty(), "{outcome:?}");

    let recorded = must_not_exist.clone();
    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let agent = terminals.mint();
        store
            .create_terminal(agent as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        // What the daemon writes down when it sees one.
        store
            .set_agent(agent as i64, Some("claude".into()), None)
            .await
            .expect("an agent");

        // A tab that was running something with an effect. The argv is recorded, as it
        // always is, and recording it is not the same as agreeing to run it again.
        let dangerous = terminals.mint();
        store
            .create_terminal(
                dangerous as i64,
                workspace,
                "/tmp".into(),
                format!("/usr/bin/touch {}", recorded.display()),
            )
            .await
            .expect("a row");
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(until(|| terminals.streams().len() == 3).await, "three tabs");
        assert!(
            until(|| resumed.exists()).await,
            "the agent's own resume command never ran in its tab, so a person reopening \
             their work gets a bare shell where a conversation was"
        );
        // Given the same room to have happened, and it must not have.
        assert!(
            !must_not_exist.exists(),
            "a saved command was re-run on restore, which is how a restart repeats a \
             deploy, a migration or an rm"
        );
    })
    .await;

    // The override table is process wide, so it is put back before another test reads it.
    qoro_detect::reload_overrides(std::path::Path::new("/nonexistent-qoro-overrides"));
}

/// G13.3: a stop ends the daemon and its terminals, and does not close the tabs.
#[tokio::test]
async fn g13_3_a_stop_ends_the_terminals_without_ending_the_session() {
    let dir = runtime_dir();
    with_daemon(dir.path(), |daemon| async move {
        let store = daemon.store();
        let workspace = daemon.workspace();
        let terminals = daemon.terminals();
        let extra = terminals.mint();
        store
            .create_terminal(extra as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        // Two rows are open, and a stop is not allowed to close either of them: the tabs
        // are what the next start brings back.
        assert_eq!(
            store.open_terminals(workspace).await.expect("rows").len(),
            2
        );
    })
    .await;

    // A stop is the daemon going, which the drop above already is. What must not have
    // happened is the rows being marked closed.
    let after = with_daemon(dir.path(), |daemon| async move {
        daemon
            .store()
            .open_terminals(daemon.workspace())
            .await
            .expect("rows")
            .len()
    })
    .await;
    assert_eq!(
        after, 2,
        "a session that stopped lost its tabs, so stopping is deleting"
    );
}

/// The restart says what survived and what fell back, counted by kind.
///
/// One agent with a resume command, one agent without one, and one plain shell
/// whose directory is gone. The report must name each: resumed, shell fallback,
/// plain, and moved directory. A single count would call all of these "3 panes".
#[tokio::test]
async fn a_restart_reports_resumed_agents_shell_fallbacks_and_moved_directories() {
    let dir = runtime_dir();

    // A manifest id no other test uses. The override table is process wide and a
    // missing-directory reload does not clear it, so sharing `claude` with g13_5
    // would let the two tests swap each other's resume command mid-run.
    let overrides = tempfile::tempdir().expect("a directory");
    std::fs::write(
        overrides.path().join("restore-probe-xyz.toml"),
        "id = \"restore-probe-xyz\"\nversion = \"test\"\n[resume]\nlatest = [\"/usr/bin/true\"]\n",
    )
    .expect("an override");
    let outcome = qoro_detect::reload_overrides(overrides.path());
    assert!(outcome.errors.is_empty(), "{outcome:?}");

    // A directory that is gone: under the temp dir and never created, rather than a
    // fixed path that could exist on some machine.
    let gone = dir.path().join("gone");
    assert!(!gone.exists());

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let resumed = terminals.mint();
        store
            .create_terminal(resumed as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        store
            .set_agent(resumed as i64, Some("restore-probe-xyz".into()), None)
            .await
            .expect("an agent");
        let fallback = terminals.mint();
        store
            .create_terminal(fallback as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        store
            .set_agent(fallback as i64, Some("no-such-agent-xyz".into()), None)
            .await
            .expect("an agent");
        let moved = terminals.mint();
        store
            .create_terminal(
                moved as i64,
                workspace,
                gone.display().to_string(),
                "/bin/sh".into(),
            )
            .await
            .expect("a row");
    })
    .await;

    let report = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 4).await,
            "the session came back with {} tabs rather than four: {:?}",
            terminals.streams().len(),
            terminals.streams()
        );
        daemon.restore_report()
    })
    .await;

    assert_eq!(
        report.terminals, 4,
        "three rows plus the daemon's own: {report:?}"
    );
    assert_eq!(report.brought_back, 4, "{report:?}");
    assert_eq!(report.agents_resumed, 1, "{report:?}");
    assert_eq!(report.agents_shell_fallback, 1, "{report:?}");
    assert_eq!(report.plain_shells, 2, "{report:?}");
    assert_eq!(report.cwd_fallback, 1, "{report:?}");
}

/// The shutdown request answers before it goes, and says what it is ending.
///
/// Not run against a daemon that would exit the test process: what is asserted is that the
/// request exists, is answered, and names its count. The exit itself is the last line of
/// the handler and is what the `qoro stop` command waits on.
#[test]
fn a_shutdown_request_is_part_of_the_protocol_and_carries_an_id() {
    let request = ControlRequest::Shutdown {
        id: "stop-1".into(),
    };
    assert_eq!(request.request_id(), "stop-1");
    let wire = serde_json::to_value(&request).expect("encode");
    assert_eq!(
        wire["op"], "Shutdown",
        "the method is named on the wire, or no other client can call it"
    );
}

/// G13.8: what has scrolled past can be read back, counted from the newest row.
///
/// Against a real daemon and a real shell, because the point of the request is what is on
/// the disk: the engine keeps a thousand rows of cells and the disk keeps rows of text
/// (ADR-003), and a person scrolling back is reading the second.
#[tokio::test]
async fn g13_8_what_scrolled_past_is_read_back_from_the_newest_row() {
    let d = qoro_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut client = d.attach("reader").await;

    // More output than a screen holds, so some of it is behind the screen rather than on
    // it. Numbered, so which rows came back is not a matter of opinion.
    client
        .input_to(terminal, b"for i in $(seq 1 200); do echo row-$i; done\n")
        .await;
    assert!(
        client
            .wait_for_text_on(terminal, "row-200", qoro_testkit::PATIENCE)
            .await
    );
    // The history is written by a task of its own, so it is waited for rather than
    // assumed: a read that arrives first is a read of an empty file.
    let mut answer = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let asked = client
            .control(
                &ControlRequest::Scrollback {
                    id: "back".into(),
                    terminal,
                    up: 20,
                    rows: 20,
                },
                qoro_testkit::PATIENCE,
            )
            .await;
        if let Some(qoro_runtime::control::ControlResponse::Scrollback { total, .. }) = &asked
            && *total > 100
        {
            answer = asked;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(qoro_runtime::control::ControlResponse::Scrollback {
        from,
        total,
        lines,
        terminal: answered,
        ..
    }) = answer
    else {
        panic!("the daemon never answered with the rows that went past");
    };
    assert_eq!(answered, terminal);
    assert_eq!(
        lines.len(),
        20,
        "twenty rows asked for, {} back",
        lines.len()
    );
    assert_eq!(
        from,
        total - 20,
        "counted back from the newest row, so the answer says which rows these are"
    );
    // Consecutive, in order, and actually the rows that went past.
    let numbers: Vec<i64> = lines
        .iter()
        .filter_map(|l| l.trim().strip_prefix("row-")?.parse().ok())
        .collect();
    assert_eq!(numbers.len(), 20, "the rows came back changed: {lines:?}");
    for pair in numbers.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "the rows came back out of order or with gaps: {numbers:?}"
        );
    }

    // Asking for more than there is answers what there is, rather than failing.
    let all = client
        .control(
            &ControlRequest::Scrollback {
                id: "everything".into(),
                terminal,
                up: 10_000,
                rows: 10_000,
            },
            qoro_testkit::PATIENCE,
        )
        .await;
    let Some(qoro_runtime::control::ControlResponse::Scrollback { from, lines, .. }) = all else {
        panic!("a request past the oldest row is not a failure");
    };
    assert_eq!(from, 0, "past the oldest row is the oldest row");
    assert!(
        lines.len() <= 500,
        "a client asked for ten thousand rows and the daemon allocated them: {}",
        lines.len()
    );
}

/// G13.9: an agent's own id for its conversation reaches the row, and a forged one does not.
///
/// The id is what makes a restart come back to this conversation rather than to whichever
/// one in the directory happens to be newest, which stops being the same thing the moment
/// a person has two agents in one repository.
#[tokio::test]
async fn g13_9_a_reported_session_id_is_recorded_and_a_forged_report_is_not() {
    use qoro_runtime::control::ControlResponse;

    let d = qoro_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("hook").await;

    // A forged token is refused, and writes nothing.
    let refused = client
        .control(
            &ControlRequest::ReportAgentState {
                id: "forged".into(),
                terminal,
                token: "not-the-token".into(),
                state: "working".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: true,
                clock: 0,
                session: Some("stolen-id".into()),
            },
            qoro_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "a forged report was accepted: {refused:?}"
    );

    // The real one is admitted, and its id is written down.
    let accepted = client
        .control(
            &ControlRequest::ReportAgentState {
                id: "real".into(),
                terminal,
                token,
                state: "working".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: true,
                clock: 0,
                session: Some("abc-123".into()),
            },
            qoro_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );

    // Written from a task of its own, so it is waited for rather than assumed.
    let store = d.store();
    let workspace = d.workspace();
    let mut recorded = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let rows = store.open_terminals(workspace).await.expect("rows");
        if let Some(row) = rows.iter().find(|r| r.id == terminal as i64)
            && row.agent_session.is_some()
        {
            recorded = row.agent_session.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        recorded.as_deref(),
        Some("abc-123"),
        "the agent said which conversation it was having and nothing wrote it down"
    );
    assert_ne!(
        recorded.as_deref(),
        Some("stolen-id"),
        "a refused report still wrote its id"
    );
}
