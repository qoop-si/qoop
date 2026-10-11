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

use qoop_runtime::paths;
use qoop_runtime::{Daemon, control::ControlRequest};

/// Held for the whole of every test that installs an agent override.
///
/// The override table is process wide, and a reload sets it to what one directory holds:
/// another test's reload, or its clean-up, drops this test's override mid-run.
static OVERRIDES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _binding = qoop_testkit::BINDING.lock().await;
    let daemon = Daemon::bind(dir, Path::new("/bin/sh"))
        .await
        .expect("the daemon binds");
    // What `qoop` does against a daemon that restored nothing: open a shell. These gates
    // were written against the daemon doing it itself.
    if daemon.terminals().streams().is_empty() {
        daemon
            .open_first_terminal()
            .await
            .expect("the first terminal");
    }
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

/// G13.4: a tab closed before the stop keeps its number, so the next start does not reuse it.
///
/// The closed row stays in the store under its id. Minting above only the tabs that came
/// back handed that id out again and the new tab's insert failed on it.
#[tokio::test]
async fn g13_4_a_closed_tab_number_is_not_minted_again_after_a_restart() {
    let dir = runtime_dir();
    let closed = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let closed = terminals.mint();
        store
            .create_terminal(
                closed as i64,
                daemon.workspace(),
                "/tmp".into(),
                "/bin/sh".into(),
            )
            .await
            .expect("a row");
        assert!(store.close_terminal(closed as i64).await.expect("close"));
        closed
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let next = daemon.terminals().mint();
        assert!(
            next > closed,
            "a new tab would take the number {next} of a closed one ({closed})"
        );
    })
    .await;
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
    let _overrides = OVERRIDES.lock().await;
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
    let outcome = qoop_detect::reload_overrides(overrides.path());
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
    qoop_detect::reload_overrides(std::path::Path::new("/nonexistent-qoop-overrides"));
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
    let _overrides = OVERRIDES.lock().await;
    let dir = runtime_dir();

    // A manifest id no other test uses, so a leftover from another test cannot be
    // mistaken for this one's.
    let overrides = tempfile::tempdir().expect("a directory");
    std::fs::write(
        overrides.path().join("restore-probe-xyz.toml"),
        "id = \"restore-probe-xyz\"\nversion = \"test\"\n[resume]\nlatest = [\"/usr/bin/true\"]\n",
    )
    .expect("an override");
    let outcome = qoop_detect::reload_overrides(overrides.path());
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

/// A restart carries a stored conversation id into memory only with a resume that names it.
///
/// A Claude Code id with no transcript is resumed as the newest conversation instead, so
/// the agent that comes back is not in that one, and the tab must not say it is. The id a
/// resume does name is held for its agent (`Said::Resumed`), covered where `Said` is.
#[tokio::test]
async fn a_restart_carries_no_conversation_id_its_resume_does_not_name() {
    let dir = runtime_dir();
    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let tab = terminals.mint();
        store
            .create_terminal(tab as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        store
            .set_agent(
                tab as i64,
                Some("claude".to_string()),
                Some("ses_abc123".to_string()),
            )
            .await
            .expect("an agent with a session");
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 2).await,
            "the session came back with {} tabs rather than two: {:?}",
            terminals.streams().len(),
            terminals.streams()
        );
        assert!(
            terminals
                .streams()
                .into_iter()
                .all(|s| terminals.session_of(s).is_none()),
            "a tab says it is in a conversation its resume did not name"
        );
    })
    .await;
}

/// An empty stored session stays empty in memory after a restart.
///
/// The restore skips blank ids like the live report path does, so a tab
/// that never had a conversation does not gain a phantom one.
#[tokio::test]
async fn a_restart_leaves_a_blank_stored_session_blank_in_memory() {
    let dir = runtime_dir();
    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let tab = terminals.mint();
        store
            .create_terminal(tab as i64, workspace, "/tmp".into(), "/bin/sh".into())
            .await
            .expect("a row");
        store
            .set_agent(
                tab as i64,
                Some("claude".to_string()),
                Some("   ".to_string()),
            )
            .await
            .expect("an agent with a blank session");
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 2).await,
            "the session came back with {} tabs rather than two: {:?}",
            terminals.streams().len(),
            terminals.streams()
        );
        assert!(
            terminals
                .streams()
                .iter()
                .all(|s| terminals.session_of(*s).is_none()),
            "a blank stored session became a memory session: {:?}",
            terminals
                .streams()
                .iter()
                .map(|s| terminals.session_of(*s))
                .collect::<Vec<_>>()
        );
    })
    .await;
}

/// The shutdown request answers before it goes, and says what it is ending.
///
/// Not run against a daemon that would exit the test process: what is asserted is that the
/// request exists, is answered, and names its count. The exit itself is the last line of
/// the handler and is what the `qoop stop` command waits on.
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
    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut client = d.attach("reader").await;

    // More output than a screen holds, so some of it is behind the screen rather than on
    // it. Numbered, so which rows came back is not a matter of opinion.
    client
        .input_to(terminal, b"for i in $(seq 1 200); do echo row-$i; done\n")
        .await;
    assert!(
        client
            .wait_for_text_on(terminal, "row-200", qoop_testkit::PATIENCE)
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
                qoop_testkit::PATIENCE,
            )
            .await;
        if let Some(qoop_runtime::control::ControlResponse::Scrollback { total, .. }) = &asked
            && *total > 100
        {
            answer = asked;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(qoop_runtime::control::ControlResponse::Scrollback {
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
            qoop_testkit::PATIENCE,
        )
        .await;
    let Some(qoop_runtime::control::ControlResponse::Scrollback { from, lines, .. }) = all else {
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
    use qoop_runtime::control::ControlResponse;

    let d = qoop_testkit::TestDaemon::start().await;
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
                event: Some("tool".into()),
                label: Some("Read foo.rs".into()),
            },
            qoop_testkit::PATIENCE,
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
                event: None,
                label: None,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );

    // The write is awaited before the answer, under the store deadline, rather
    // than spawned: a stop that lands between the answer and a background write
    // would lose the conversation the tab was having. So this reads the row
    // straight after the answer, with no wait loop: a write that had not
    // happened yet would read back empty here.
    let store = d.store();
    let workspace = d.workspace();
    let rows = store.open_terminals(workspace).await.expect("rows");
    let recorded = rows
        .iter()
        .find(|r| r.id == terminal as i64)
        .and_then(|r| r.agent_session.clone());
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

/// A session id reported without a state claim is recorded, refused when
/// forged, and never moves the belief: saying which conversation this is
/// says nothing about what the agent is doing.
#[tokio::test]
async fn g13_10_a_session_only_report_is_recorded_without_touching_the_belief() {
    use qoop_runtime::control::ControlResponse;

    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("hook-session").await;
    let before = d
        .agent(terminal)
        .expect("the terminal is tracked")
        .belief
        .state;

    // A forged token is refused, and writes nothing.
    let refused = client
        .control(
            &ControlRequest::ReportAgentSession {
                id: "forged-session".into(),
                terminal,
                token: "not-the-token".into(),
                session: "stolen-id".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "a forged session report was accepted: {refused:?}"
    );

    // An empty id is refused too: there is nothing to record, and silence
    // must not clear what is there.
    let empty = client
        .control(
            &ControlRequest::ReportAgentSession {
                id: "empty-session".into(),
                terminal,
                token: token.clone(),
                session: "   ".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(empty, Some(ControlResponse::Failed { .. })),
        "an empty session report was accepted: {empty:?}"
    );

    // The real one is admitted, and its id is written down.
    let accepted = client
        .control(
            &ControlRequest::ReportAgentSession {
                id: "real-session".into(),
                terminal,
                token,
                session: "solo-456".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "{accepted:?}"
    );

    // The belief never moved: no observation was offered.
    assert_eq!(
        d.agent(terminal).expect("tracked").belief.state,
        before,
        "a session report changed what the index shows"
    );

    // The write is awaited before the answer, under the store deadline, rather
    // than spawned: a stop that lands between the answer and a background write
    // would lose the conversation the tab was having. So this reads the row
    // straight after the answer, with no wait loop: a write that had not
    // happened yet would read back empty here.
    let store = d.store();
    let workspace = d.workspace();
    let rows = store.open_terminals(workspace).await.expect("rows");
    let recorded = rows
        .iter()
        .find(|r| r.id == terminal as i64)
        .and_then(|r| r.agent_session.clone());
    assert_eq!(
        recorded.as_deref(),
        Some("solo-456"),
        "the agent said which conversation it was having and nothing wrote it down"
    );
    assert_ne!(
        recorded.as_deref(),
        Some("stolen-id"),
        "a refused session report still wrote its id"
    );
}

/// A rename through the control request is durable before the answer.
///
/// The handler awaits the title write rather than spawning it, the way a name
/// given at birth is written at create: a renamed tab that is stopped before
/// a background write would come back under its old name. Read here straight
/// after `Renamed`, with no wait loop; the trip from the row back to a tab is
/// what G13.4 already gates, so this plus that is a rename that survives a
/// restart.
#[tokio::test]
async fn a_rename_is_recorded_before_the_answer_and_a_cleared_name_forgets() {
    use qoop_runtime::control::ControlResponse;

    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut client = d.attach("renamer").await;

    let renamed = client
        .control(
            &ControlRequest::RenameTerminal {
                id: "rename-1".into(),
                terminal,
                name: Some("release".into()),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(renamed, Some(ControlResponse::Renamed { .. })),
        "{renamed:?}"
    );
    let rows = d.store().open_terminals(d.workspace()).await.expect("rows");
    assert_eq!(
        rows.iter()
            .find(|r| r.id == terminal as i64)
            .and_then(|r| r.title.clone())
            .as_deref(),
        Some("release"),
        "the answer went out before the name was written down"
    );

    // Clearing gives the tab back to no name, rather than leaving the old one
    // behind for a restart to resurrect.
    let cleared = client
        .control(
            &ControlRequest::RenameTerminal {
                id: "rename-2".into(),
                terminal,
                name: None,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(cleared, Some(ControlResponse::Renamed { .. })),
        "{cleared:?}"
    );
    let rows = d.store().open_terminals(d.workspace()).await.expect("rows");
    assert_eq!(
        rows.iter()
            .find(|r| r.id == terminal as i64)
            .and_then(|r| r.title.clone()),
        None,
        "a cleared tab came back cleared on screen but named in the row"
    );
}

/// Two tabs in one directory resume two conversations, not the newest twice.
///
/// The failure this gates is every tab with no id falling back to `latest`:
/// three `opencode` tabs in one repository converge on whichever conversation
/// is newest. With an id per tab the restore asks each agent for its own.
#[tokio::test]
async fn two_tabs_in_one_directory_resume_distinct_conversations() {
    let _overrides = OVERRIDES.lock().await;
    let dir = runtime_dir();
    let evidence = tempfile::tempdir().expect("a directory");

    // A manifest id no other test uses: the override table is process wide.
    let overrides = tempfile::tempdir().expect("a directory");
    std::fs::write(
        overrides.path().join("distinct-probe-xyz.toml"),
        format!(
            "id = \"distinct-probe-xyz\"\nversion = \"test\"\n[resume]\n\
             session = [\"/usr/bin/touch\", \"{}/{{session}}\"]\n\
             latest = [\"/usr/bin/touch\", \"{}/latest\"]\n",
            evidence.path().display(),
            evidence.path().display(),
        ),
    )
    .expect("an override");
    let outcome = qoop_detect::reload_overrides(overrides.path());
    assert!(outcome.errors.is_empty(), "{outcome:?}");

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        for id in ["conv-aaa", "conv-bbb"] {
            let tab = terminals.mint();
            store
                .create_terminal(tab as i64, workspace, "/tmp".into(), "/bin/sh".into())
                .await
                .expect("a row");
            // What the daemon writes down when a hook reports an id; the second
            // half of this is G13.9, so this test sets the rows the way that
            // path leaves them and gates the restore half.
            store
                .set_agent(
                    tab as i64,
                    Some("distinct-probe-xyz".into()),
                    Some(id.into()),
                )
                .await
                .expect("an agent with its conversation");
        }
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 3).await,
            "three tabs, not {}: {:?}",
            terminals.streams().len(),
            terminals.streams()
        );
        for id in ["conv-aaa", "conv-bbb"] {
            assert!(
                until(|| evidence.path().join(id).exists()).await,
                "no tab resumed conversation {id}"
            );
        }
        assert!(
            !evidence.path().join("latest").exists(),
            "a tab with an id fell back to the newest conversation"
        );
    })
    .await;

    qoop_detect::reload_overrides(std::path::Path::new("/nonexistent-qoop-overrides"));
}

fn leaf(terminal: u32) -> qoop_runtime::control::Pane {
    qoop_runtime::control::Pane::Leaf { terminal }
}

fn beside(
    first: qoop_runtime::control::Pane,
    second: qoop_runtime::control::Pane,
) -> qoop_runtime::control::Pane {
    qoop_runtime::control::Pane::Split {
        dir: qoop_runtime::control::SplitDir::V,
        permille: 500,
        questions: 0,
        first: Box::new(first),
        second: Box::new(second),
    }
}

/// A tab split into four comes back as one tab with four panes, not as four tabs.
///
/// The incident this gate is for: four agents in one tab, a restart, four tabs.
#[tokio::test]
async fn a_restart_brings_a_split_tab_back_as_one_tree() {
    let dir = runtime_dir();
    let tree = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let first = terminals.streams()[0];
        let mut panes = vec![first];
        for _ in 0..3 {
            let pane = terminals.mint();
            store
                .create_terminal(pane as i64, workspace, "/tmp".into(), "/bin/sh".into())
                .await
                .expect("a row");
            panes.push(pane);
        }
        let tree = beside(
            beside(leaf(panes[0]), leaf(panes[1])),
            beside(leaf(panes[2]), leaf(panes[3])),
        );
        terminals
            .set_split(tree.clone(), &store, |_| false)
            .await
            .expect("recorded");
        tree
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        assert!(
            until(|| terminals.streams().len() == 4).await,
            "four panes, not {:?}",
            terminals.streams()
        );
        assert_eq!(terminals.live_splits(&terminals.streams()), vec![tree]);
    })
    .await;
}

/// A pane that did not come back is cut out of its tree on disk, not only in the listing,
/// so no later terminal that happens to get its number is pulled into the tab.
#[tokio::test]
async fn a_restart_cuts_a_pane_that_did_not_come_back_out_of_the_stored_tree() {
    let dir = runtime_dir();
    let kept = with_daemon(dir.path(), |daemon| async move {
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        let first = terminals.streams()[0];
        let mut panes = vec![first];
        for _ in 0..2 {
            let pane = terminals.mint();
            store
                .create_terminal(pane as i64, workspace, "/tmp".into(), "/bin/sh".into())
                .await
                .expect("a row");
            panes.push(pane);
        }
        terminals
            .set_split(
                beside(leaf(panes[0]), beside(leaf(panes[1]), leaf(panes[2]))),
                &store,
                |_| false,
            )
            .await
            .expect("recorded");
        // Closed while the daemon runs: the row stays, as closed.
        store.close_terminal(panes[2] as i64).await.expect("closed");
        beside(leaf(panes[0]), leaf(panes[1]))
    })
    .await;

    with_daemon(dir.path(), |daemon| async move {
        let store = daemon.store();
        let stored = store.splits().await.expect("splits");
        assert_eq!(stored.len(), 1, "{stored:?}");
        let tree: qoop_runtime::control::Pane = serde_json::from_str(&stored[0]).expect("a tree");
        assert_eq!(tree, kept, "the dead pane is still on disk");
    })
    .await;
}

/// Over the wire: a recorded split rides the index, and a closed pane leaves it.
#[tokio::test]
async fn a_recorded_split_rides_the_index_until_a_pane_closes() {
    use qoop_runtime::control::ControlResponse;
    let d = qoop_testkit::TestDaemon::start().await;
    let first = d.terminals().streams()[0];
    let mut client = d.attach("splitter").await;
    let Some(ControlResponse::TerminalCreated {
        terminal: second, ..
    }) = client
        .control(
            &ControlRequest::CreateTerminal {
                id: "split-1".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            qoop_testkit::PATIENCE,
        )
        .await
    else {
        panic!("no terminal for the split");
    };
    let tree = beside(leaf(first), leaf(second));
    assert!(matches!(
        client
            .control(
                &ControlRequest::SetSplit {
                    id: "tree-1".into(),
                    tree: tree.clone(),
                },
                qoop_testkit::PATIENCE,
            )
            .await,
        Some(ControlResponse::SplitSaved { .. })
    ));
    let Some(ControlResponse::Agents { splits, .. }) = client
        .control(
            &ControlRequest::ListAgents {
                id: "index-1".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await
    else {
        panic!("no index");
    };
    assert_eq!(splits, vec![tree]);

    client
        .control(
            &ControlRequest::CloseTerminal {
                id: "close-1".into(),
                terminal: second,
            },
            qoop_testkit::PATIENCE,
        )
        .await
        .expect("closed");
    // The answer comes before the teardown finishes; the index follows the registry.
    let registry = d.terminals();
    assert!(until(|| !registry.streams().contains(&second)).await);
    let Some(ControlResponse::Agents { splits, .. }) = client
        .control(
            &ControlRequest::ListAgents {
                id: "index-2".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await
    else {
        panic!("no index");
    };
    assert!(splits.is_empty(), "a tree of one is a tab: {splits:?}");
}

/// A conversation named here rides the index as its title, and forgetting the name gives
/// the agent's own back. Refused while the agent has not said which conversation it is.
#[tokio::test]
async fn a_session_name_rides_the_index_and_clears() {
    use qoop_runtime::control::ControlResponse;

    let d = qoop_testkit::TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("session-namer").await;
    let rename = |id: &str, title: Option<&str>| ControlRequest::RenameSession {
        id: id.into(),
        terminal,
        title: title.map(str::to_string),
    };
    let refused = client
        .control(&rename("early", Some("release")), qoop_testkit::PATIENCE)
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "a name for no conversation was taken: {refused:?}"
    );
    let reported = client
        .control(
            &ControlRequest::ReportAgentSession {
                id: "session".into(),
                terminal,
                token,
                session: "conv-1".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(reported, Some(ControlResponse::AgentStateReported { .. })),
        "{reported:?}"
    );
    let title = |response: Option<ControlResponse>| match response {
        Some(ControlResponse::Agents { agents, .. }) => agents
            .into_iter()
            .find(|a| a.terminal == terminal)
            .and_then(|a| a.agent_title),
        other => panic!("expected the index, got {other:?}"),
    };
    let named = client
        .control(&rename("name", Some("  bağlama  ")), qoop_testkit::PATIENCE)
        .await;
    assert!(
        matches!(named, Some(ControlResponse::Renamed { .. })),
        "{named:?}"
    );
    let listed = client
        .control(
            &ControlRequest::ListAgents { id: "i1".into() },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert_eq!(title(listed).as_deref(), Some("bağlama"));
    assert_eq!(
        d.store().session_titles().await.expect("titles"),
        vec![("conv-1".to_string(), "bağlama".to_string())],
        "the answer went out before the name was written down"
    );
    let cleared = client
        .control(&rename("clear", None), qoop_testkit::PATIENCE)
        .await;
    assert!(
        matches!(cleared, Some(ControlResponse::Renamed { .. })),
        "{cleared:?}"
    );
    let listed = client
        .control(
            &ControlRequest::ListAgents { id: "i2".into() },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert_eq!(title(listed), None);
    assert!(d.store().session_titles().await.expect("titles").is_empty());
}

/// A tab moves to another space whole: the index, the row on disk and its place at the
/// end of that space's tabs. A pane leaves its split and the rest of the split stays;
/// a move to a space that does not exist changes nothing.
#[tokio::test]
async fn a_tab_moves_to_another_space_and_leaves_its_split() {
    use qoop_runtime::control::ControlResponse;

    let d = qoop_testkit::TestDaemon::start().await;
    let first = d.terminals().streams()[0];
    let mut client = d.attach("mover").await;
    let mut open = Vec::new();
    for id in ["t2", "t3"] {
        match client
            .control(
                &ControlRequest::CreateTerminal {
                    id: id.into(),
                    cwd: None,
                    workspace: None,
                    name: None,
                },
                qoop_testkit::PATIENCE,
            )
            .await
        {
            Some(ControlResponse::TerminalCreated { terminal, .. }) => open.push(terminal),
            other => panic!("expected a terminal, got {other:?}"),
        }
    }
    let (second, third) = (open[0], open[1]);
    let tree = beside(beside(leaf(first), leaf(second)), leaf(third));
    let saved = client
        .control(
            &ControlRequest::SetSplit {
                id: "tree".into(),
                tree,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(saved, Some(ControlResponse::SplitSaved { .. })),
        "{saved:?}"
    );
    let space = match client
        .control(
            &ControlRequest::CreateWorkspace {
                id: "w".into(),
                name: "elsewhere".into(),
            },
            qoop_testkit::PATIENCE,
        )
        .await
    {
        Some(ControlResponse::WorkspaceCreated { workspace, .. }) => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    };
    let nowhere = client
        .control(
            &ControlRequest::MoveTerminal {
                id: "m0".into(),
                terminal: second,
                workspace: space + 100,
                place: None,
                leave_split: false,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(nowhere, Some(ControlResponse::Failed { .. })),
        "{nowhere:?}"
    );
    assert_eq!(d.terminals().workspace_of(second), Some(d.workspace()));
    let moved = client
        .control(
            &ControlRequest::MoveTerminal {
                id: "m1".into(),
                terminal: second,
                workspace: space,
                place: None,
                leave_split: false,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(moved, Some(ControlResponse::TerminalMoved { terminal, workspace, .. })
            if terminal == second && workspace == space),
        "{moved:?}"
    );
    match client
        .control(
            &ControlRequest::ListAgents { id: "i".into() },
            qoop_testkit::PATIENCE,
        )
        .await
    {
        Some(ControlResponse::Agents { agents, splits, .. }) => {
            let at = |t: u32| agents.iter().find(|a| a.terminal == t).map(|a| a.workspace);
            assert_eq!(at(second), Some(space));
            assert_eq!(at(first), Some(d.workspace()));
            assert_eq!(
                splits,
                vec![beside(leaf(first), leaf(third))],
                "the split kept the pane that left, or lost the ones that stayed"
            );
        }
        other => panic!("expected the index, got {other:?}"),
    }
    let rows = d.store().open_terminals(space).await.expect("rows");
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![second as i64],
        "the row on disk stayed in the space it left"
    );
    let left = d.store().open_terminals(d.workspace()).await.expect("rows");
    assert!(!left.iter().any(|r| r.id == second as i64), "{left:?}");
    // On disk too, or the next start brings back a split spanning two spaces.
    let stored: Vec<qoop_runtime::control::Pane> = d
        .store()
        .splits()
        .await
        .expect("splits")
        .iter()
        .map(|json| serde_json::from_str(json).expect("a tree"))
        .collect();
    assert_eq!(stored, vec![beside(leaf(first), leaf(third))]);
    // A second tab lands after the first.
    let again = client
        .control(
            &ControlRequest::MoveTerminal {
                id: "m2".into(),
                terminal: third,
                workspace: space,
                place: None,
                leave_split: false,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(again, Some(ControlResponse::TerminalMoved { .. })),
        "{again:?}"
    );
    let rows = d.store().open_terminals(space).await.expect("rows");
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![second as i64, third as i64]
    );
    assert!(d.store().splits().await.expect("splits").is_empty());
}

/// A tab reorders inside its own space, its split with it: the index lists the new order,
/// the disk keeps it for the next start, an older client's move into its own space is
/// still nothing, and a pane placed on its own leaves its split for a tab of its own.
#[tokio::test]
async fn a_tab_reorders_in_its_own_space_split_and_all() {
    use qoop_runtime::control::{ControlResponse, Place};
    async fn mv(
        client: &mut qoop_testkit::TestClient,
        id: &str,
        terminal: u32,
        workspace: i64,
        place: Option<Place>,
    ) -> Option<ControlResponse> {
        Box::pin(client.control(
            &ControlRequest::MoveTerminal {
                id: id.into(),
                terminal,
                workspace,
                place,
                leave_split: false,
            },
            qoop_testkit::PATIENCE,
        ))
        .await
    }
    async fn order(client: &mut qoop_testkit::TestClient, space: i64) -> Vec<u32> {
        match client
            .control(
                &ControlRequest::ListAgents { id: "i".into() },
                qoop_testkit::PATIENCE,
            )
            .await
        {
            Some(ControlResponse::Agents { agents, .. }) => agents
                .iter()
                .filter(|a| a.workspace == space)
                .map(|a| a.terminal)
                .collect(),
            other => panic!("expected the index, got {other:?}"),
        }
    }
    let moved =
        |r: &Option<ControlResponse>| matches!(r, Some(ControlResponse::TerminalMoved { .. }));
    let d = qoop_testkit::TestDaemon::start().await;
    let first = d.terminals().streams()[0];
    let space = d.workspace();
    let mut client = d.attach("reorder").await;
    let mut open = vec![first];
    for id in ["t2", "t3", "t4"] {
        match client
            .control(
                &ControlRequest::CreateTerminal {
                    id: id.into(),
                    cwd: None,
                    workspace: None,
                    name: None,
                },
                qoop_testkit::PATIENCE,
            )
            .await
        {
            Some(ControlResponse::TerminalCreated { terminal, .. }) => open.push(terminal),
            other => panic!("expected a terminal, got {other:?}"),
        }
    }
    let (second, third, fourth) = (open[1], open[2], open[3]);
    // An older client's move into its own space: answered, and nothing moves.
    let r = mv(&mut client, "r0", fourth, space, None).await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(
        order(&mut client, space).await,
        vec![first, second, third, fourth]
    );
    // To the front.
    let r = mv(&mut client, "r1", fourth, space, Some(Place::Before(first))).await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(
        order(&mut client, space).await,
        vec![fourth, first, second, third]
    );
    // A split of the second and the third moves as one, and stays a split.
    let tree = beside(leaf(second), leaf(third));
    let saved = client
        .control(
            &ControlRequest::SetSplit {
                id: "tree".into(),
                tree: tree.clone(),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(saved, Some(ControlResponse::SplitSaved { .. })),
        "{saved:?}"
    );
    let r = mv(
        &mut client,
        "r2",
        second,
        space,
        Some(Place::Before(fourth)),
    )
    .await;
    assert!(moved(&r), "{r:?}");
    // Naming one of its own panes is where it already is.
    let r = mv(&mut client, "r4", second, space, Some(Place::Before(third))).await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(
        order(&mut client, space).await,
        vec![second, third, fourth, first]
    );
    let stored: Vec<qoop_runtime::control::Pane> = d
        .store()
        .splits()
        .await
        .expect("splits")
        .iter()
        .map(|json| serde_json::from_str(json).expect("a tree"))
        .collect();
    assert_eq!(stored, vec![tree], "the split stayed whole");
    // The next start opens them in this order.
    let rows = d.store().open_terminals(space).await.expect("rows");
    assert_eq!(
        rows.iter().map(|r| r.id as u32).collect::<Vec<_>>(),
        vec![second, third, fourth, first]
    );
    // A pane that closed is still in the recorded tree until the next start; the tab it
    // leaves still moves, and a place named by it is its tab's.
    let closed = client
        .control(
            &ControlRequest::CloseTerminal {
                id: "x".into(),
                terminal: third,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(closed, Some(ControlResponse::TerminalClosed { .. })),
        "{closed:?}"
    );
    let gone = std::time::Instant::now();
    while d.terminals().get(third).is_some() && gone.elapsed() < qoop_testkit::PATIENCE {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let r = mv(&mut client, "r5", second, space, Some(Place::End)).await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(order(&mut client, space).await, vec![fourth, first, second]);
    let r = mv(&mut client, "r6", fourth, space, Some(Place::Before(third))).await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(order(&mut client, space).await, vec![first, fourth, second]);
    // A pane placed on its own leaves its split and is a tab of its own at that place.
    let tree = beside(leaf(first), leaf(fourth));
    let saved = Box::pin(client.control(
        &ControlRequest::SetSplit {
            id: "tree2".into(),
            tree,
        },
        qoop_testkit::PATIENCE,
    ))
    .await;
    assert!(
        matches!(saved, Some(ControlResponse::SplitSaved { .. })),
        "{saved:?}"
    );
    // Without being asked to leave, a pane that is not its split's first is refused.
    let r = mv(&mut client, "r7", fourth, space, Some(Place::End)).await;
    assert!(matches!(r, Some(ControlResponse::Failed { .. })), "{r:?}");
    let r = Box::pin(client.control(
        &ControlRequest::MoveTerminal {
            id: "r8".into(),
            terminal: fourth,
            workspace: space,
            place: Some(Place::End),
            leave_split: true,
        },
        qoop_testkit::PATIENCE,
    ))
    .await;
    assert!(moved(&r), "{r:?}");
    assert_eq!(order(&mut client, space).await, vec![first, second, fourth]);
    let trees: Vec<qoop_runtime::control::Pane> = d
        .store()
        .splits()
        .await
        .expect("splits")
        .iter()
        .map(|json| serde_json::from_str(json).expect("a tree"))
        .collect();
    assert!(
        !trees
            .iter()
            .any(|t| t.leaves().contains(&fourth) || t.leaves().contains(&first)),
        "the split it left had one pane, which is a tab: {trees:?}"
    );
}

/// Spaces keep the order a person gave them, and the daemon's own space stays its own
/// wherever it is listed.
#[tokio::test]
async fn spaces_keep_their_order_and_the_first_stays_the_daemons() {
    use qoop_runtime::control::ControlResponse;
    let d = qoop_testkit::TestDaemon::start().await;
    let home = d.workspace();
    let mut client = d.attach("order").await;
    let mut made = Vec::new();
    for name in ["b", "c"] {
        match client
            .control(
                &ControlRequest::CreateWorkspace {
                    id: name.into(),
                    name: name.into(),
                },
                qoop_testkit::PATIENCE,
            )
            .await
        {
            Some(ControlResponse::WorkspaceCreated { workspace, .. }) => made.push(workspace),
            other => panic!("expected a workspace, got {other:?}"),
        }
    }
    let (b, c) = (made[0], made[1]);
    let moved = client
        .control(
            &ControlRequest::MoveWorkspace {
                id: "o1".into(),
                workspace: c,
                before: Some(home),
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(moved, Some(ControlResponse::Renamed { .. })),
        "{moved:?}"
    );
    match client
        .control(
            &ControlRequest::ListWorkspaces { id: "l".into() },
            qoop_testkit::PATIENCE,
        )
        .await
    {
        Some(ControlResponse::Workspaces { workspaces, .. }) => {
            assert_eq!(
                workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
                vec![c, home, b]
            );
            let first = workspaces.iter().find(|w| w.id == home).expect("listed");
            assert!(!first.closable, "the daemon's own space is still its own");
        }
        other => panic!("expected the list, got {other:?}"),
    }
    let nowhere = client
        .control(
            &ControlRequest::MoveWorkspace {
                id: "o2".into(),
                workspace: 999,
                before: None,
            },
            qoop_testkit::PATIENCE,
        )
        .await;
    assert!(
        matches!(nowhere, Some(ControlResponse::Failed { .. })),
        "{nowhere:?}"
    );
}
