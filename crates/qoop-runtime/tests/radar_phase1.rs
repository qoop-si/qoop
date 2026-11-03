//! Radar phase 1 gates: the daemon stamps activity and exposes cwd on the row.
//!
//! A hook report that changes something stamps the row with the daemon's own
//! clock; a session report never moves the stamp; a forgery moves nothing.
//! The row carries where the terminal is, seeded at creation and refreshed
//! by the terminal's own task, never an empty string. All clocks here are
//! the daemon's: the tests assert order and presence, never an exact time.

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestDaemon};

async fn agents_of(client: &mut qoop_testkit::TestClient, id: &str) -> Vec<AgentRow> {
    match client
        .control(&ControlRequest::ListAgents { id: id.into() }, PATIENCE)
        .await
    {
        Some(ControlResponse::Agents { agents, .. }) => agents,
        other => panic!("an agents listing was expected: {other:?}"),
    }
}

fn row_of(rows: &[AgentRow], terminal: u32) -> &AgentRow {
    rows.iter()
        .find(|r| r.terminal == terminal)
        .unwrap_or_else(|| panic!("no row for terminal {terminal}: {rows:?}"))
}

/// G8: an accepted hook report stamps the row; a session-only report and a
/// forgery leave the stamp exactly where it was.
#[tokio::test]
async fn hook_report_stamps_and_session_report_and_forgery_do_not() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("hook").await;

    let before = row_of(&agents_of(&mut client, "l0").await, terminal).last_active_at_unix;
    let accepted = client
        .control(
            &ControlRequest::ReportAgentState {
                from: None,
                id: "r1".into(),
                terminal,
                token: token.clone(),
                state: "blocked".into(),
                visible_blocker: true,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: None,
                label: None,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(accepted, Some(ControlResponse::AgentStateReported { .. })),
        "an admitted hook report was refused: {accepted:?}"
    );
    let stamped = row_of(&agents_of(&mut client, "l1").await, terminal)
        .last_active_at_unix
        .expect("an accepted hook report stamps the row");
    assert!(
        before.is_none_or(|at| stamped >= at),
        "the stamp moved backwards: {before:?} to {stamped}"
    );
    // The hook was really applied, not just admitted: the row says blocked.
    // This is also what makes the equalities below stable rather than
    // racy. A hook-held blocker outranks the shell's idle screens, so no
    // background evaluation can accept a change and move the stamp while
    // the row reads blocked.
    assert_eq!(
        row_of(&agents_of(&mut client, "l1b").await, terminal).state,
        "blocked"
    );

    // A session report is identity without a state claim: admitted, stored,
    // and the stamp does not move.
    let sessioned = client
        .control(
            &ControlRequest::ReportAgentSession {
                from: None,
                id: "s1".into(),
                terminal,
                token: token.clone(),
                session: "ses_only_identity_no_claim".into(),
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(sessioned, Some(ControlResponse::AgentStateReported { .. })),
        "an admitted session report was refused: {sessioned:?}"
    );
    assert_eq!(
        row_of(&agents_of(&mut client, "l2").await, terminal).last_active_at_unix,
        Some(stamped),
        "a session report moved the activity stamp"
    );

    // A forgery is refused and moves nothing.
    let forged = client
        .control(
            &ControlRequest::ReportAgentState {
                from: None,
                id: "f1".into(),
                terminal,
                token: "not-the-token".into(),
                state: "working".into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: None,
                label: None,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(forged, Some(ControlResponse::Failed { .. })),
        "a forged report was accepted: {forged:?}"
    );
    assert_eq!(
        row_of(&agents_of(&mut client, "l3").await, terminal).last_active_at_unix,
        Some(stamped),
        "a forgery moved the activity stamp"
    );
}

/// G9: the row carries where the terminal is. Seeded at creation, present on
/// the daemon's own first terminal, equal to the asked directory on an
/// explicit create, and never an empty string on any row.
#[tokio::test]
async fn the_row_carries_where_the_terminal_is_and_never_blank() {
    let d = TestDaemon::start().await;
    let mut client = d.attach("places").await;

    let dir = std::env::temp_dir().join(format!("qoop-radar-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let wanted = dir
        .canonicalize()
        .expect("canonical scratch")
        .display()
        .to_string();
    let created = client
        .control(
            &ControlRequest::CreateTerminal {
                id: "mk".into(),
                cwd: Some(wanted.clone()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    let terminal = match created {
        Some(ControlResponse::TerminalCreated { terminal, .. }) => terminal,
        other => panic!("a terminal was expected: {other:?}"),
    };

    let rows = agents_of(&mut client, "l1").await;
    assert!(!rows.is_empty(), "ListAgents came back with no rows at all");
    for row in &rows {
        assert_ne!(
            row.cwd.as_deref(),
            Some(""),
            "terminal {} carries a blank cwd",
            row.terminal
        );
    }
    assert_eq!(
        row_of(&rows, terminal).cwd.as_deref(),
        Some(wanted.as_str()),
        "the created terminal is not where it was asked to start"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A terminal started with no directory is where the daemon is, and says so
/// from birth. Unseeded, a tab that never moved stayed `cwd: None` for good,
/// which `qoop worktree rm` has to read as "may be inside" and refuse.
#[tokio::test]
async fn a_terminal_started_without_a_directory_is_where_the_daemon_is() {
    let d = TestDaemon::start().await;
    let mut client = d.attach("here").await;
    let created = client
        .control(
            &ControlRequest::CreateTerminal {
                id: "mk".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await;
    let terminal = match created {
        Some(ControlResponse::TerminalCreated { terminal, .. }) => terminal,
        other => panic!("a terminal was expected: {other:?}"),
    };
    let here = std::env::current_dir()
        .expect("a directory")
        .display()
        .to_string();
    let rows = agents_of(&mut client, "l1").await;
    assert_eq!(
        row_of(&rows, terminal).cwd.as_deref(),
        Some(here.as_str()),
        "a terminal made without a directory has none on its row"
    );
}

/// A `cd` on a quiet prompt reaches the row. The shell prints its prompt within the
/// throttle window of the echo before it, and nothing after that: a read skipped by the
/// throttle has to happen once the window lapses, or the row keeps the old directory for
/// good, which is what let `qoop worktree rm` believe a worktree was empty.
#[tokio::test]
async fn a_fast_cd_on_a_quiet_prompt_reaches_the_row() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut client = d.attach("cd").await;
    let dir = tempfile::tempdir().expect("a scratch directory");
    let wanted = dir
        .path()
        .canonicalize()
        .expect("canonical scratch")
        .display()
        .to_string();
    client
        .input_to(terminal, format!("cd '{wanted}'\n").as_bytes())
        .await;
    let deadline = std::time::Instant::now() + PATIENCE;
    let mut seen = None;
    while std::time::Instant::now() < deadline {
        seen = row_of(&agents_of(&mut client, "poll").await, terminal)
            .cwd
            .clone();
        if seen.as_deref() == Some(wanted.as_str()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the row still says {seen:?} after a quiet cd to {wanted}");
}

/// The listing's row carries the stop the daemon recorded: a turn's end names one, a second
/// turn end a new one, and work after it takes it away.
#[tokio::test]
async fn the_row_names_the_stop_of_each_turn_end() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut client = d.attach("stop").await;
    let mut report = async |id: &str, state: &str, event: &str| {
        let r = client
            .control(
                &ControlRequest::ReportAgentState {
                    from: None,
                    id: id.into(),
                    terminal,
                    token: token.clone(),
                    state: state.into(),
                    visible_blocker: false,
                    visible_idle: false,
                    visible_working: false,
                    clock: 0,
                    session: None,
                    event: Some(event.into()),
                    label: None,
                },
                PATIENCE,
            )
            .await;
        assert!(
            matches!(r, Some(ControlResponse::AgentStateReported { .. })),
            "{r:?}"
        );
        row_of(&agents_of(&mut client, &format!("{id}-ls")).await, terminal)
            .stop
            .clone()
    };
    assert_eq!(
        report("s1", "working", "user").await,
        None,
        "no turn has ended"
    );
    let first = report("s2", "idle", "done").await.expect("a stop");
    assert!(first.ends_with(".1"), "{first}");
    let second = report("s3", "idle", "done").await.expect("a stop");
    assert_ne!(second, first, "each turn end is a new stop");
    assert_eq!(report("s4", "working", "tool").await, None, "work ends it");
}
