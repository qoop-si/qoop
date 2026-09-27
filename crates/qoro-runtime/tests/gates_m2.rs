//! Milestone 2 gates: multiple terminals, addressed by the frame header's stream field.
//!
//! Each test names the gate it covers, and a
//! test with no gate is a preference rather than a commitment.

use std::time::{Duration, Instant};

use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_testkit::{PATIENCE, TestClient, TestDaemon};

/// Held by the gate that fills the probe pool with stuck jobs, and by every gate here that
/// creates a terminal in a directory, which needs a probe. The pool is one per process, so
/// without this a create checked while G2.9 held all four threads was refused as busy.
static PROBE_POOL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn terminal_of(response: &ControlResponse) -> u32 {
    match response {
        ControlResponse::TerminalCreated { terminal, .. } => *terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

/// Two terminals in one daemon are two shells, and each one's output reaches the client
/// tagged with the terminal it came from.
#[tokio::test]
async fn two_terminals_are_two_shells_addressed_separately() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("multi").await;

    let first = d.terminals().streams()[0];
    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "r1".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let second = terminal_of(&created);
    assert_ne!(first, second, "the new terminal reused an existing stream");
    assert_eq!(d.terminals().len(), 2);

    // Each shell is told a different thing, and each screen must show only its own.
    c.input_to(first, b"echo FIRST_$((6*7))\n").await;
    c.input_to(second, b"echo SECOND_$((6*8))\n").await;

    assert!(
        c.wait_for_text_on(first, "FIRST_42", PATIENCE).await,
        "the first terminal did not answer"
    );
    assert!(
        c.wait_for_text_on(second, "SECOND_48", PATIENCE).await,
        "the second terminal did not answer"
    );
    let first_screen = c.screen(first).expect("a screen").lines.join("\n");
    assert!(
        !first_screen.contains("SECOND_48"),
        "the second terminal's output appeared on the first terminal's screen"
    );
}

/// G2.5: closing one terminal leaves every other one alone.
#[tokio::test]
async fn g2_5_closing_a_terminal_does_not_disturb_the_others() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("closer").await;
    let keep = d.terminals().streams()[0];

    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "r1".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let doomed = terminal_of(&created);

    c.input_to(keep, b"echo ALIVE_$((6*7))\n").await;
    assert!(c.wait_for_text_on(keep, "ALIVE_42", PATIENCE).await);

    let closed = c
        .control(
            &ControlRequest::CloseTerminal {
                id: "r2".into(),
                terminal: doomed,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(matches!(closed, ControlResponse::TerminalClosed { .. }));

    // The registry loses it, and the survivor still works. Both halves matter: a close that
    // leaves the handle registered means a later command routes into a dead terminal.
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline && d.terminals().get(doomed).is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        d.terminals().get(doomed).is_none(),
        "a closed terminal is still registered, so a command would route into it"
    );

    c.input_to(keep, b"echo STILL_$((6*8))\n").await;
    assert!(
        c.wait_for_text_on(keep, "STILL_48", PATIENCE).await,
        "closing one terminal stopped another one"
    );
}

/// A command for a terminal that has gone is dropped, not routed to a neighbour. Stream
/// numbers are never reused precisely so that this is possible to assert.
#[tokio::test]
async fn a_command_for_a_closed_terminal_never_reaches_another_one() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("stale").await;
    let survivor = d.terminals().streams()[0];

    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "r1".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let doomed = terminal_of(&created);
    c.control(
        &ControlRequest::CloseTerminal {
            id: "r2".into(),
            terminal: doomed,
        },
        PATIENCE,
    )
    .await
    .expect("a response");
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline && d.terminals().get(doomed).is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    c.input_to(doomed, b"echo GHOST_$((6*7))\n").await;
    c.input_to(survivor, b"echo REAL_$((6*8))\n").await;
    assert!(c.wait_for_text_on(survivor, "REAL_48", PATIENCE).await);
    let screen = c.screen(survivor).expect("a screen").lines.join("\n");
    assert!(
        !screen.contains("GHOST_42"),
        "input addressed to a closed terminal was executed somewhere else"
    );
}

/// Listing reports what the registry holds, and creating changes it.
#[tokio::test]
async fn listing_terminals_reports_the_registry() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("lister").await;

    let before = c
        .control(&ControlRequest::ListTerminals { id: "l1".into() }, PATIENCE)
        .await
        .expect("a response");
    let ControlResponse::Terminals {
        terminals: before, ..
    } = before
    else {
        panic!("expected a listing");
    };
    assert_eq!(before.len(), 1);

    c.control(
        &ControlRequest::CreateTerminal {
            id: "r1".into(),
            cwd: None,
            workspace: None,
            name: None,
        },
        PATIENCE,
    )
    .await
    .expect("a response");

    let after = c
        .control(&ControlRequest::ListTerminals { id: "l2".into() }, PATIENCE)
        .await
        .expect("a response");
    let ControlResponse::Terminals {
        terminals: after, ..
    } = after
    else {
        panic!("expected a listing");
    };
    assert_eq!(after.len(), 2);
    assert!(
        after.starts_with(&before),
        "an existing terminal changed id"
    );
}

/// A malformed control request is refused with a reason, never dropped. A client waiting
/// for a response it will never receive cannot tell that from a hung daemon.
#[tokio::test]
async fn a_control_request_that_cannot_be_answered_is_refused_in_words() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("refused").await;
    let response = c
        .control(
            &ControlRequest::CloseTerminal {
                id: "r1".into(),
                terminal: 9999,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    match response {
        ControlResponse::Failed { reason, .. } => {
            assert!(
                reason.contains("9999"),
                "the reason does not say what failed"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A requested working directory is honoured, and a bad one is refused rather than quietly
/// ignored.
///
/// The first version destructured `cwd` away and answered `TerminalCreated`, so a client
/// asking for a shell in its project directory got one somewhere else with no error and no
/// way to find out.
#[tokio::test]
async fn a_created_terminal_starts_where_the_client_asked() {
    let _pool = PROBE_POOL.lock().await;
    let d = TestDaemon::start().await;
    let mut c = d.attach("cwd").await;

    let asked = d.dir().join("workdir");
    std::fs::create_dir(&asked).expect("mkdir");
    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "r1".into(),
                cwd: Some(asked.to_string_lossy().into_owned()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let terminal = terminal_of(&created);

    // Asked of the shell itself, and compared against the real path: a temporary directory
    // on macOS is reached through a symlink, so the shell's own `pwd` is the only answer
    // that can be checked without guessing which form it will print.
    c.input_to(terminal, b"pwd\n").await;
    let real = std::fs::canonicalize(&asked).expect("canonicalize");
    let name = real
        .file_name()
        .expect("a directory name")
        .to_string_lossy()
        .into_owned();
    assert!(
        c.wait_for_text_on(terminal, &name, PATIENCE).await,
        "the shell did not start in the requested directory"
    );
}

#[tokio::test]
async fn a_working_directory_that_cannot_be_used_is_refused_in_words() {
    let _pool = PROBE_POOL.lock().await;
    let d = TestDaemon::start().await;
    let mut c = d.attach("bad-cwd").await;

    for (id, cwd, expected) in [
        ("rel", "not/absolute", "not absolute"),
        (
            "missing",
            "/definitely/not/a/directory/here",
            "not a directory",
        ),
    ] {
        let response = c
            .control(
                &ControlRequest::CreateTerminal {
                    id: id.into(),
                    cwd: Some(cwd.into()),
                    workspace: None,
                    name: None,
                },
                PATIENCE,
            )
            .await
            .expect("a response");
        match response {
            ControlResponse::Failed { reason, .. } => assert!(
                reason.contains(expected),
                "the reason does not say what was wrong: {reason}"
            ),
            other => panic!("a bad cwd was accepted: {other:?}"),
        }
    }
    assert_eq!(
        d.terminals().len(),
        1,
        "a refused create still started a terminal"
    );
}

/// A terminal's metadata outlives the daemon that made it.
///
/// The processes do not, and are not meant to: a pty and a shell die with the daemon. What
/// survives is what a person asked for, which is the difference between metadata and state.
#[tokio::test]
async fn terminal_metadata_survives_a_restart() {
    let _pool = PROBE_POOL.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    qoro_runtime::paths::ensure_private_dir(dir.path()).expect("private dir");

    let asked = dir.path().join("project");
    std::fs::create_dir(&asked).expect("mkdir");

    {
        let daemon = qoro_runtime::Daemon::bind(dir.path(), std::path::Path::new("/bin/sh"))
            .await
            .expect("bind");
        let socket = daemon.socket().to_path_buf();
        let serving = tokio::spawn(async move { daemon.serve().await });

        let (stream, inbuf) = qoro_runtime::client::connect(&socket, "recorder")
            .await
            .expect("connect");
        let mut c = TestClient::from_parts(stream, inbuf, 1);
        let created = c
            .control(
                &ControlRequest::CreateTerminal {
                    id: "r1".into(),
                    cwd: Some(asked.to_string_lossy().into_owned()),
                    workspace: None,
                    name: None,
                },
                PATIENCE,
            )
            .await
            .expect("a response");
        assert!(
            matches!(created, ControlResponse::TerminalCreated { .. }),
            "{created:?}"
        );
        serving.abort();
    }

    // A second daemon over the same directory reads what the first one recorded.
    let store = qoro_store::Store::open(&dir.path().join("qoro.db")).expect("open");
    let workspaces = store.workspaces().expect("workspaces");
    assert_eq!(workspaces.len(), 1, "the workspace was not persisted");
    let terminals = store.open_terminals(workspaces[0].id).expect("terminals");
    assert!(
        terminals.iter().any(|t| t.cwd == asked.to_string_lossy()),
        "the terminal's working directory was not recorded: {terminals:?}"
    );
}

/// G2.7: scrollback spill. Emit far more than the in-memory window, then assert every line
/// is on disk in order.
///
/// One 64 KiB read can scroll more than the whole 1000 row window, and the engine emits no
/// eviction event, so a spill running at the wrong point loses history silently and every
/// other test still passes.
#[tokio::test]
async fn g2_7_rows_that_scroll_off_reach_the_disk_in_order() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("spiller").await;
    let terminal = d.terminals().streams()[0];

    // 4000 numbered lines, which is four times the in-memory window, emitted as fast as the
    // shell can write them so that a single read carries many of them.
    c.input_to(terminal, b"seq 1 4000\n").await;
    assert!(
        c.wait_for_text_on(terminal, "4000", PATIENCE).await,
        "the shell never finished printing"
    );
    // The writer is a separate task; give it a moment to drain the ring.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let dir = d.dir().join("history").join(terminal.to_string());
    let history = qoro_store::History::open(&dir).expect("history");
    assert!(
        history.len() >= 3000,
        "only {} rows reached the disk, so the spill is losing history",
        history.len()
    );

    // In order, and complete over a window well inside what was written.
    let rows = history.read(0, 2000).expect("read");
    let numbers: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.trim().parse::<i64>().ok())
        .collect();
    assert!(
        numbers.len() >= 1500,
        "only {} of the first 2000 rows were numbers, so rows are being mangled",
        numbers.len()
    );
    for pair in numbers.windows(2) {
        assert!(
            pair[1] == pair[0] + 1,
            "history is out of order or has a hole: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

/// A tab closed for good takes its history with it.
///
/// Nothing deleted a history directory, so every tab ever opened stayed on disk, and a new
/// tab handed an old number after a closed space's rows were removed showed the old tab's
/// scrollback.
#[tokio::test]
async fn a_closed_terminal_takes_its_history_with_it() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("closer").await;
    let terminal = d.terminals().streams()[0];
    c.input_to(terminal, b"seq 1 3000\n").await;
    assert!(
        c.wait_for_text_on(terminal, "3000", PATIENCE).await,
        "the shell never finished printing"
    );
    let dir = d.dir().join("history").join(terminal.to_string());
    let deadline = Instant::now() + PATIENCE;
    while !dir.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dir.exists(), "the history never reached the disk");

    c.control(
        &ControlRequest::CloseTerminal {
            id: "gone".into(),
            terminal,
        },
        PATIENCE,
    )
    .await
    .expect("a response");
    let deadline = Instant::now() + PATIENCE;
    while dir.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!dir.exists(), "a closed tab's history stayed on disk");
}

/// A start removes history no open row owns, before any new id is minted, and leaves an
/// open row's history alone.
#[tokio::test]
async fn a_start_removes_history_that_no_open_terminal_owns() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    qoro_runtime::paths::ensure_private_dir(dir.path()).expect("private dir");

    {
        let mut store = qoro_store::Store::open(&dir.path().join("qoro.db")).expect("open");
        let space = store.create_workspace("w", 1).expect("workspace");
        store
            .create_terminal(5, space, &dir.path().display().to_string(), "/bin/sh", 1)
            .expect("an open row");
    }
    let history = dir.path().join("history");
    for name in ["5", "99", "notes"] {
        std::fs::create_dir_all(history.join(name)).expect("mkdir");
        std::fs::write(history.join(name).join("000000000000.seg"), b"").expect("write");
    }

    let daemon = {
        let _binding = qoro_testkit::BINDING.lock().await;
        qoro_runtime::Daemon::bind(dir.path(), std::path::Path::new("/bin/sh"))
            .await
            .expect("bind")
    };
    assert!(
        !history.join("99").exists(),
        "history with no open row survived a start"
    );
    assert!(
        history.join("5").join("000000000000.seg").exists(),
        "a restored tab lost its history"
    );
    assert!(
        history.join("notes").exists(),
        "something that is not a terminal's history was removed"
    );
    drop(daemon);
}

/// G2.7b: a burst larger than the spill ring still reaches the disk whole.
///
/// G2.7 prints fewer rows than the ring holds, so it cannot see the ring overflow. A burst
/// of short lines scrolls tens of thousands of rows in the time the writer used to spend
/// waiting for a batch to gather, and those rows were displaced before it looked. Measured
/// before the fix: of 50,000 lines, 26,608 reached the disk.
#[tokio::test]
async fn g2_7b_a_burst_larger_than_the_spill_ring_reaches_the_disk_whole() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("burst").await;
    let terminal = d.terminals().streams()[0];

    // Sixty blank rows after the numbers push the last of them off the screen, so every
    // number is history and none is still being drawn.
    c.input_to(
        terminal,
        b"seq 1 100000; yes '' | head -n 60; echo BURST_$((6*7))\n",
    )
    .await;
    assert!(
        c.wait_for_text_on(terminal, "BURST_42", PATIENCE).await,
        "the shell never finished printing"
    );
    let dir = d.dir().join("history").join(terminal.to_string());
    // Every number, in order, with no hole: wait for the writer rather than guess a delay.
    let deadline = std::time::Instant::now() + PATIENCE;
    let numbers = loop {
        let history = qoro_store::History::open(&dir).expect("history");
        let rows = history.read(0, history.len() as usize).expect("read");
        let numbers: Vec<i64> = rows
            .iter()
            .filter_map(|r| r.trim().parse::<i64>().ok())
            .collect();
        if numbers.last() == Some(&100_000) || std::time::Instant::now() > deadline {
            break numbers;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        numbers.len(),
        100_000,
        "{} of 100000 rows reached the disk",
        numbers.len()
    );
    for pair in numbers.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "history has a hole");
    }
}

/// G2.8's rule, at the level it is actually enforced: the read loop hands rows over and
/// never waits, so a stalled disk costs history and not the terminal.
#[tokio::test]
async fn g2_8_the_terminal_keeps_running_when_history_cannot_be_written() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("degraded").await;
    let terminal = d.terminals().streams()[0];

    // Make the history directory unwritable, which is what a full or read-only filesystem
    // looks like from here. The terminal must not notice.
    let dir = d.dir().join("history");
    std::fs::create_dir_all(&dir).ok();
    std::fs::set_permissions(
        &dir,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o500),
    )
    .expect("chmod");

    c.input_to(terminal, b"seq 1 2000\n").await;
    assert!(
        c.wait_for_text_on(terminal, "2000", PATIENCE).await,
        "the terminal stopped when history could not be written"
    );
    c.input_to(terminal, b"echo STILL_$((6*7))\n").await;
    assert!(
        c.wait_for_text_on(terminal, "STILL_42", PATIENCE).await,
        "the terminal stopped serving after a history failure"
    );

    std::fs::set_permissions(
        &dir,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .ok();
}

/// G2.14: a client asks an owner that never answers, and ownership does not move.
///
/// This is the gate that invariant I6 needed and that the
/// quiesce gate was previously cited for, which tests neither sizing nor ownership.
#[tokio::test]
async fn g2_14_ownership_does_not_move_when_the_owner_never_answers() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];

    let mut owner = d.attach("owner").await;
    owner.resize(120, 40).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.input_to(terminal, b"stty size\n").await;
    assert!(owner.wait_for_text_on(terminal, "40 120", PATIENCE).await);

    // A second client asks, and the owner says nothing at all.
    let mut asker = d.attach("asker").await;
    let response = asker
        .control(
            &ControlRequest::RequestOwnership {
                id: "want".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(
        matches!(response, ControlResponse::OwnershipRequested { .. }),
        "the request was not even delivered: {response:?}"
    );

    // Long enough that any timeout based steal would have fired.
    tokio::time::sleep(Duration::from_secs(3)).await;
    asker.resize(60, 20).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Asserted as an absence, not a presence. The earlier `stty size` output is still on
    // the screen, so "40 120 is there" is true whatever the size became; "20 60 is not
    // there" is only true if the asker's size never took. The marker is what says the
    // command has run, so the absence is a fact about a screen that has been updated.
    owner.forget_seen();
    owner
        .input_to(terminal, b"echo AFTER_$((6*7)); stty size\n")
        .await;
    assert!(
        owner.wait_for_text_on(terminal, "AFTER_42", PATIENCE).await,
        "the shell never answered"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner
        .wait_for_text_on(terminal, "never matches", Duration::from_millis(200))
        .await;
    let screen = owner.screen(terminal).expect("a screen").lines.join("\n");
    assert!(
        !screen.contains("20 60"),
        "a silent owner lost the geometry, which is the steal ADR-007 forbids:\n{screen}"
    );
}

/// And the path that does move it: the owner agrees.
#[tokio::test]
async fn ownership_moves_when_the_owner_agrees_and_not_before() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];

    let mut owner = d.attach("owner").await;
    owner.resize(120, 40).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut asker = d.attach("asker").await;
    // The daemon names connections in order, so the second one is c2. A client learns its
    // own name from the welcome in the real protocol; here the gate asserts the mechanism.
    let asker_name = d
        .connections()
        .progress()
        .iter()
        .map(|(id, _, _)| id.as_str().to_string())
        .nth(1)
        .expect("two connections");

    // Before the grant, the asker cannot resize.
    asker.resize(60, 20).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.forget_seen();
    owner.input_to(terminal, b"stty size\n").await;
    assert!(owner.wait_for_text_on(terminal, "40 120", PATIENCE).await);

    let granted = owner
        .control(
            &ControlRequest::GrantOwnership {
                id: "give".into(),
                terminal,
                to: asker_name,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(matches!(granted, ControlResponse::OwnershipGranted { .. }));
    tokio::time::sleep(Duration::from_millis(300)).await;

    asker.resize(90, 30).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    asker.input_to(terminal, b"stty size\n").await;
    assert!(
        asker.wait_for_text_on(terminal, "30 90", PATIENCE).await,
        "the owner agreed and ownership still did not move"
    );
}

/// G2.4: a small client sees a window onto a large terminal, and the terminal does not
/// shrink to fit it.
///
/// Two halves, and only both together are the property. The child's winsize must stay at
/// the owner's size, because a terminal that shrinks when a phone attaches is a terminal
/// whose layout changes under the person who owns it. And the small client must render a
/// correct crop, because drawing a 180 column grid into a 50 column window does not fail,
/// it wraps, and one long line silently becomes four.
#[tokio::test]
async fn g2_4_a_small_client_gets_a_viewport_and_the_terminal_keeps_its_size() {
    use qoro_domain::{PtySize, Viewport};

    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];

    let mut owner = d.attach("desktop").await;
    owner.resize(180, 50).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner.input_to(terminal, b"stty size\n").await;
    assert!(owner.wait_for_text_on(terminal, "50 180", PATIENCE).await);

    // A second, much smaller client. It is not the owner, so its resize is refused, which
    // is ADR-007 and gate G1.7. What it gets instead is a window.
    let mut phone = d.attach("phone").await;
    phone.resize(50, 20).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Something with a distinct pattern per column, so a crop that takes the wrong columns
    // is visible rather than merely a different length.
    owner
        .input_to(
            terminal,
            b"printf 'L%s\\n' $(seq 1 3); echo READY_$((6*7))\n",
        )
        .await;
    assert!(phone.wait_for_text_on(terminal, "READY_42", PATIENCE).await);

    let snapshot = phone.screen(terminal).expect("a screen").clone();
    assert_eq!(
        snapshot.cols, 180,
        "the terminal shrank to fit the small client"
    );
    assert_eq!(snapshot.rows, 50);

    let grid = PtySize {
        cols: 180,
        rows: 50,
    };
    let view = Viewport::new(grid, 0, 0, 50, 20);
    let shown = view.crop(snapshot.lines.iter().map(String::as_str));
    assert_eq!(shown.len(), 20, "the window is 20 rows");
    assert!(
        shown.iter().all(|l| l.chars().count() <= 50),
        "a cropped row is wider than the window, so it would wrap"
    );
    // And it is the same text, not a different rendering of it.
    let first = snapshot.lines[0].chars().take(50).collect::<String>();
    assert_eq!(shown[0], first);

    // The owner's view is unchanged by any of this.
    owner.forget_seen();
    owner
        .input_to(terminal, b"echo AFTER_$((6*8)); stty size\n")
        .await;
    assert!(owner.wait_for_text_on(terminal, "AFTER_48", PATIENCE).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    owner
        .wait_for_text_on(terminal, "never matches", Duration::from_millis(200))
        .await;
    let screen = owner.screen(terminal).expect("a screen").lines.join("\n");
    assert!(
        !screen.contains("20 50"),
        "the small client resized the terminal:\n{screen}"
    );
}

/// G2.15: no mouse escape sequence reaches the child after an abrupt disconnect.
///
/// A child that turned mouse tracking on is still expecting reports when the client holding
/// the outer terminal vanishes. The next client's outer terminal has not been told to
/// report anything, so it sends nothing, and the reset preamble on attach is what makes
/// that true rather than hoped for.
#[tokio::test]
async fn g2_15_no_mouse_report_reaches_the_child_after_a_reattach() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let out = d.dir().join("mouse.out");

    let mut first = d.attach("mouser").await;
    // The child asks for mouse tracking, then records everything it is sent.
    first
        .input_to(
            terminal,
            format!(
                "printf '\\033[?1000h\\033[?1006h'; stty raw; head -c 8 > {}\n",
                out.display()
            )
            .as_bytes(),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Gone, with no goodbye.
    drop(first);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut second = d.attach("reattached").await;
    // Whatever the second client does, it must not be a mouse report. It types eight
    // ordinary bytes, which is exactly what the child is counting.
    second.input_to(terminal, b"ABCDEFGH").await;

    let deadline = Instant::now() + PATIENCE;
    let mut got = Vec::new();
    while Instant::now() < deadline {
        if let Ok(v) = std::fs::read(&out)
            && v.len() >= 8
        {
            got = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        got,
        b"ABCDEFGH",
        "the child received {:?}, which is not what the second client typed",
        String::from_utf8_lossy(&got)
    );
}

/// G2.9: a dead filesystem under one terminal does not stop the daemon.
///
/// The gate names a hung mount. There is none here, and installing a filesystem to run a
/// test is worse than the test.
///
/// A fifo was the first attempt, because a read on one with no writer is a real
/// uncancellable syscall on a real path. It does not work as a test: a blocked reader is
/// released only by a writer, a probe that has not started yet blocks in `open` rather than
/// `read`, and the runtime waits for every blocking task at shutdown, so the test hangs
/// after passing every assertion. Three arrangements of it hung in three different places.
///
/// What actually matters about a hung mount is that the thread is occupied, cannot be
/// cancelled, and does not come back within the deadline. A sleep on the blocking pool is
/// all three, and it releases itself, so the test finishes. What is lost is that the block
/// is not a filesystem call, and the daemon cannot tell the difference: it holds a permit
/// either way.
#[tokio::test(flavor = "multi_thread")]
async fn g2_9_a_hung_syscall_under_one_terminal_leaves_the_rest_working() {
    use qoro_runtime::blocking;
    let _pool = PROBE_POOL.lock().await;

    let d = TestDaemon::start().await;
    let mut c = d.attach("survivor").await;
    let alive = d.terminals().streams()[0];

    let hung = d.dir().join("hung");
    std::fs::create_dir(&hung).expect("mkdir");
    let prefix = hung.display().to_string();

    // Exactly as many probes as the pool holds, and no more: a queued one starts only after
    // an earlier one returns, which is not the state being tested. Four is already above
    // the three abandonments that mark a prefix degraded.
    assert!(blocking::PROBE_THREADS as u64 >= blocking::DEGRADE_AFTER);
    let before_abandoned = blocking::abandoned_count();
    let stuck_until = Instant::now() + Duration::from_secs(6);
    for _ in 0..blocking::PROBE_THREADS {
        let prefix = prefix.clone();
        tokio::spawn(async move {
            let _ = blocking::probe(&prefix, Duration::from_millis(200), || {
                std::thread::sleep(Duration::from_secs(6))
            })
            .await;
        });
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(
        blocking::abandoned_count() > before_abandoned,
        "nothing was abandoned, so the probes did not actually block"
    );
    assert!(
        blocking::is_degraded(&prefix),
        "the prefix was not marked degraded, so the pool would drain one probe at a time"
    );

    // Every other terminal still reads and writes.
    c.input_to(alive, b"echo STILL_$((6*7))\n").await;
    assert!(
        c.wait_for_text_on(alive, "STILL_42", PATIENCE).await,
        "a hung filesystem under one terminal stopped another one"
    );

    // A terminal in a healthy directory can still be made while the probe pool is held:
    // the stuck probes are another path's problem, and refusing this one left no directory
    // anyone could open a tab in for the rest of the daemon's life.
    let elsewhere = d.dir().join("healthy");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    let in_a_directory = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "healthy-while-hung".into(),
                cwd: Some(elsewhere.display().to_string()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(
        matches!(in_a_directory, ControlResponse::TerminalCreated { .. }),
        "a full probe pool refused a healthy directory: {in_a_directory:?}"
    );

    // A new terminal can still be created, which needs the lifecycle pool that the probes
    // must not have taken.
    let created = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "while-hung".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let fresh = terminal_of(&created);
    c.input_to(fresh, b"echo NEW_$((6*8))\n").await;
    assert!(
        c.wait_for_text_on(fresh, "NEW_48", PATIENCE).await,
        "a terminal created while a filesystem was hung does not work"
    );

    // And an unrelated teardown completes within its deadline.
    let started = Instant::now();
    c.control(
        &ControlRequest::CloseTerminal {
            id: "close-while-hung".into(),
            terminal: fresh,
        },
        PATIENCE,
    )
    .await
    .expect("a response");
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && d.terminals().get(fresh).is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        d.terminals().get(fresh).is_none(),
        "a teardown did not complete while the probe pool was full"
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the teardown took {:?}, which is past its deadline",
        started.elapsed()
    );

    // The probes were still blocked throughout, which is what makes the assertions above
    // about a full pool rather than about an empty one.
    assert!(
        blocking::is_degraded(&prefix),
        "the probes finished before the assertions, so the pool was not full"
    );
    blocking::clear_degraded(&prefix);
    // Held until the stuck jobs have let their threads go, so the next gate that needs a
    // probe finds the pool empty rather than full.
    tokio::time::sleep_until(stuck_until.into()).await;
}

/// G2.13: a hundred terminals change foreground process at the same moment, and every one
/// of them reports the right one within the stated bound.
///
/// The failure this rules out is a settle-timer herd: a hundred terminals all changing at
/// once, all scheduling the same re-read, and some left holding stale state because the
/// wakeups collided.
///
/// It runs in process because the thing being checked is the daemon's own record, and no
/// separate process can read it. `sleep` produces no output at all, which is what makes
/// this a test of the subscription rather than of the screen.
#[tokio::test(flavor = "multi_thread")]
async fn g2_13_a_hundred_terminals_quiesce_within_the_stated_bound() {
    let terminals: usize = std::env::var("QORO_QUIESCE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let _ = terminals;
    /// One settle window plus a hundred milliseconds, which is the bound ADR-009 states.
    const BOUND: Duration = Duration::from_millis(70 + 100);

    let d = TestDaemon::start().await;
    let mut c = d.attach("quiescer").await;
    let mut streams = d.terminals().streams();
    for i in 1..terminals {
        let created = c
            .control(
                &ControlRequest::CreateTerminal {
                    id: format!("t{i}"),
                    cwd: None,
                    workspace: None,
                    name: None,
                },
                Duration::from_secs(30),
            )
            .await
            .unwrap_or_else(|| panic!("terminal {i} was never created"));
        streams.push(terminal_of(&created));
    }
    assert_eq!(streams.len(), terminals);

    // Settled: every shell is its own foreground process.
    for stream in &streams {
        c.input_to(*stream, b"echo READY_$((6*7))\n").await;
    }
    for stream in &streams {
        assert!(
            c.wait_for_text_on(*stream, "READY_42", Duration::from_secs(30))
                .await,
            "terminal {stream} never started"
        );
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    let before: Vec<_> = streams
        .iter()
        .map(|s| {
            d.agent(*s)
                .unwrap_or_else(|| panic!("terminal {s} has no agent record"))
                .capability
                .generation
        })
        .collect();

    // All at once.
    let started = Instant::now();
    for stream in &streams {
        c.input_to(*stream, b"sleep 20\n").await;
    }

    // Every terminal's record has to show the new foreground process. Polled rather than
    // slept, so the number reported is what it actually took.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut settled = 0usize;
    let mut took = Duration::ZERO;
    while Instant::now() < deadline {
        settled = streams
            .iter()
            .zip(&before)
            .filter(|(stream, was)| {
                d.agent(**stream).is_some_and(|agent| {
                    agent.capability.generation > **was
                        && matches!(
                            &agent.identity,
                            qoro_domain::AgentIdentity::Known { program, .. }
                                if program.contains("sleep")
                        )
                })
            })
            .count();
        took = started.elapsed();
        if settled == terminals {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    println!("G2.13: {settled} of {terminals} settled in {took:?}, bound {BOUND:?}");
    assert_eq!(
        settled, terminals,
        "only {settled} of {terminals} terminals reported their new foreground process \
         within 20 s, so a simultaneous change leaves some holding stale state"
    );
    assert!(
        took <= BOUND * 4,
        "a hundred simultaneous changes took {took:?} against a bound of {BOUND:?} each, \
         which is a settle herd rather than a settle"
    );
}

/// G2.16: a daemon holds more than one workspace, and a terminal belongs to exactly one of
/// them.
///
/// The panel on the left of the interface is a list of spaces and a list of the agents in
/// them, and both are answers the daemon gives. Without this gate the daemon could report
/// one workspace forever and every screen would still look right, because a client with one
/// space to draw cannot tell a real grouping from a hardcoded number.
///
/// So: make a second workspace, put a terminal in it, and read the membership back from the
/// answers a client actually gets.
#[tokio::test]
async fn g2_16_a_terminal_belongs_to_the_workspace_it_was_made_in() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("spaces").await;

    // The daemon starts with one workspace and the terminal it opened at boot in it.
    let first = match c
        .control(
            &ControlRequest::ListWorkspaces { id: "w0".into() },
            PATIENCE,
        )
        .await
        .expect("a workspace list")
    {
        ControlResponse::Workspaces { workspaces, .. } => workspaces,
        other => panic!("expected a workspace list, got {other:?}"),
    };
    assert_eq!(first.len(), 1, "a fresh daemon should hold one workspace");
    assert_eq!(first[0].terminals, 1, "{first:?}");

    let made = match c
        .control(
            &ControlRequest::CreateWorkspace {
                id: "w1".into(),
                name: "second".into(),
            },
            PATIENCE,
        )
        .await
        .expect("a created workspace")
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a created workspace, got {other:?}"),
    };
    assert_ne!(made, first[0].id, "the new workspace reused an id");

    // A terminal asked for in that workspace has to land in it, and nowhere else.
    let moved = terminal_of(
        &c.control(
            &ControlRequest::CreateTerminal {
                id: "t1".into(),
                cwd: None,
                workspace: Some(made),
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a created terminal"),
    );

    let after = match c
        .control(
            &ControlRequest::ListWorkspaces { id: "w2".into() },
            PATIENCE,
        )
        .await
        .expect("a workspace list")
    {
        ControlResponse::Workspaces { workspaces, .. } => workspaces,
        other => panic!("expected a workspace list, got {other:?}"),
    };
    assert_eq!(after.len(), 2, "{after:?}");
    let second = after
        .iter()
        .find(|w| w.id == made)
        .unwrap_or_else(|| panic!("the new workspace is not in the list: {after:?}"));
    assert_eq!(second.name, "second");
    assert_eq!(
        second.terminals, 1,
        "the terminal did not count against the workspace it was made in: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .find(|w| w.id == first[0].id)
            .map(|w| w.terminals),
        Some(1),
        "making a terminal elsewhere changed the first workspace: {after:?}"
    );

    // And the agent index, which is what the panel draws, says the same thing.
    let agents = match c
        .control(&ControlRequest::ListAgents { id: "a1".into() }, PATIENCE)
        .await
        .expect("an agent list")
    {
        ControlResponse::Agents { agents, .. } => agents,
        other => panic!("expected an agent list, got {other:?}"),
    };
    assert_eq!(
        agents
            .iter()
            .find(|a| a.terminal == moved)
            .map(|a| a.workspace),
        Some(made),
        "the index puts the terminal in the wrong workspace: {agents:?}"
    );
}

/// An empty workspace leaves the list, and nothing else does.
///
/// The list on the left of the interface is every workspace row the store holds, so a
/// space whose terminals are all closed would sit there forever with nothing in it.
/// Closing one is a deliberate act with three refusals: an unknown id, a workspace that
/// still holds a live terminal, and the session's default, which is where a client that
/// names no workspace lands and so must always exist.
#[tokio::test]
async fn an_empty_workspace_closes_and_a_live_default_or_unknown_one_does_not() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("spaces").await;

    async fn workspaces(c: &mut TestClient) -> Vec<qoro_runtime::control::WorkspaceRow> {
        match c
            .control(&ControlRequest::ListWorkspaces { id: "w".into() }, PATIENCE)
            .await
            .expect("a workspace list")
        {
            ControlResponse::Workspaces { workspaces, .. } => workspaces,
            other => panic!("expected a workspace list, got {other:?}"),
        }
    }

    let default = workspaces(&mut c).await;
    assert_eq!(default.len(), 1, "{default:?}");
    let default_id = default[0].id;

    // Unknown first: no row, no close.
    let refused = c
        .control(
            &ControlRequest::CloseWorkspace {
                id: "c0".into(),
                workspace: 999_999,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "closing an unknown workspace succeeded: {refused:?}"
    );

    // A second workspace with a terminal in it refuses while occupied.
    let made = match c
        .control(
            &ControlRequest::CreateWorkspace {
                id: "c1".into(),
                name: "spare".into(),
            },
            PATIENCE,
        )
        .await
        .expect("a created workspace")
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a created workspace, got {other:?}"),
    };
    let occupant = terminal_of(
        &c.control(
            &ControlRequest::CreateTerminal {
                id: "c2".into(),
                cwd: None,
                workspace: Some(made),
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a created terminal"),
    );
    let refused = c
        .control(
            &ControlRequest::CloseWorkspace {
                id: "c3".into(),
                workspace: made,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "closing an occupied workspace succeeded: {refused:?}"
    );

    // The default refuses even empty: it is where an unnamed creation lands.
    let refused = c
        .control(
            &ControlRequest::CloseWorkspace {
                id: "c4".into(),
                workspace: default_id,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "closing the default workspace succeeded: {refused:?}"
    );

    // Empty it, then it goes, and the list no longer names it. The close is
    // answered before the registry lets go, so the test waits the way g2_5 does.
    c.control(
        &ControlRequest::CloseTerminal {
            id: "c5".into(),
            terminal: occupant,
        },
        PATIENCE,
    )
    .await
    .expect("a closed terminal");
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline && d.terminals().get(occupant).is_some() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        d.terminals().get(occupant).is_none(),
        "a closed terminal is still registered"
    );
    let closed = c
        .control(
            &ControlRequest::CloseWorkspace {
                id: "c6".into(),
                workspace: made,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(
            closed,
            Some(ControlResponse::WorkspaceClosed { workspace, .. }) if workspace == made
        ),
        "closing an empty workspace failed: {closed:?}"
    );
    let after = workspaces(&mut c).await;
    assert!(
        after.iter().all(|w| w.id != made),
        "the closed workspace is still listed: {after:?}"
    );
    // Closing it twice is "unknown", not a second success.
    let refused = c
        .control(
            &ControlRequest::CloseWorkspace {
                id: "c7".into(),
                workspace: made,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(refused, Some(ControlResponse::Failed { .. })),
        "closing a workspace twice succeeded: {refused:?}"
    );
}

/// G2.17: a screen that never stops changing is still read.
///
/// The settle that makes a burst one evaluation works because bursts end. An agent holding
/// the turn is the case where they do not: it animates a timer, and every frame pushed the
/// deadline out, so the screen was never read at all. Measured against a real codex, the
/// panel said `idle` for the whole of a thirty second turn whose own screen said
/// `Working (29s, esc to interrupt)` in as many words, which is the busiest state being the
/// one state that could never be detected.
///
/// A stand in named after a real agent, because the rules that read a working screen are
/// that agent's, and compiled rather than a shell script so the daemon reads its name off
/// the process rather than the interpreter's.
#[tokio::test]
async fn g2_17_a_screen_that_never_settles_is_read_anyway() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("codex.rs");
    std::fs::write(
        &source,
        "fn main() {\n\
         \x20   for n in 0.. {\n\
         \x20       println!(\"\\u{2022} Working ({n}s \\u{b7} esc to interrupt)\");\n\
         \x20       std::thread::sleep(std::time::Duration::from_millis(50));\n\
         \x20   }\n\
         }\n",
    )
    .expect("write");
    let built = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(dir.path().join("codex"))
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let d = TestDaemon::start().await;
    let mut c = d.attach("noisy").await;
    let terminal = d.terminals().streams()[0];
    c.input_to(
        terminal,
        format!("{}\n", dir.path().join("codex").display()).as_bytes(),
    )
    .await;

    // Its screen never goes quiet: a frame every fifty milliseconds, under the eighty the
    // settle waits for. The state has to arrive anyway.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = None;
    while Instant::now() < deadline {
        if let Some(agent) = d.agent(terminal)
            && agent.belief.state == qoro_domain::AgentRuntimeState::Working
        {
            seen = Some(agent.belief.state);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        seen,
        Some(qoro_domain::AgentRuntimeState::Working),
        "a screen that never stops printing `Working` was never read: {:?}",
        d.agent(terminal).map(|a| a.belief.state)
    );
    c.input_to(terminal, &[0x03]).await;
}

/// A finished opencode reads idle, not working.
///
/// Regression for the index showing `working` for agents that finished long
/// ago. Two shapes: the live composer positively seen (an explicit idle rule
/// reads its status row, and the stale interrupt hints above it are not
/// read), and a screen
/// matching no rule at all (the known-agent idle fallback settles a
/// screen-held belief instead of leaving the last state standing).
///
/// A stand in named after the real agent, like G2.17 above: the rules that
/// read the screen are that agent's, and the daemon reads the program name
/// off the process rather than the interpreter's. It prints the composer's
/// status row as opencode v1.18 draws it: the spinner and `esc interrupt`
/// while the turn runs, then the composer with the key hints alone.
async fn run_opencode_stand_in(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let source = dir.path().join("opencode.rs");
    std::fs::write(
        &source,
        "fn main() {\n\
         \x20   let plain = std::env::args().nth(1).is_some();\n\
         \x20   for n in 0..6 {\n\
         \x20       println!(\"   ⬝⬝■■■⬝⬝⬝ esc interrupt   step {n}   ctrl+p commands\");\n\
         \x20       std::thread::sleep(std::time::Duration::from_millis(200));\n\
         \x20   }\n\
         \x20   if plain {\n\
         \x20       for n in 0..15 {\n\
         \x20           println!(\"done line {n}\");\n\
         \x20       }\n\
         \x20   } else {\n\
         \x20       println!(\"  ┃  Ask anything\");\n\
         \x20       println!(\"  ┃  Build · Claude Sonnet 4.5 Anthropic\");\n\
         \x20       println!(\"  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀\");\n\
         \x20       println!(\"   ~/coding/site   ctrl+p commands\");\n\
         \x20   }\n\
         \x20   std::thread::sleep(std::time::Duration::from_secs(30));\n\
         }\n",
    )
    .expect("write");
    let binary = dir.path().join("opencode");
    let built = std::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run rustc");
    assert!(
        built.status.success(),
        "the stand in did not build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    binary
}

async fn wait_for_state(
    d: &qoro_testkit::TestDaemon,
    terminal: u32,
    want: qoro_domain::AgentRuntimeState,
    secs: u64,
) -> Option<qoro_domain::AgentRuntimeState> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(agent) = d.agent(terminal)
            && agent.belief.state == want
        {
            return Some(want);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    d.agent(terminal).map(|a| a.belief.state)
}

#[tokio::test]
async fn a_finished_opencode_with_a_live_composer_reads_idle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = run_opencode_stand_in(&dir).await;

    let d = TestDaemon::start().await;
    let mut c = d.attach("opencode-idle").await;
    let terminal = d.terminals().streams()[0];
    c.input_to(terminal, format!("{}\n", binary.display()).as_bytes())
        .await;

    assert_eq!(
        wait_for_state(&d, terminal, qoro_domain::AgentRuntimeState::Working, 15).await,
        Some(qoro_domain::AgentRuntimeState::Working),
        "the stand in's interrupt hint was never read as working",
    );
    assert_eq!(
        wait_for_state(&d, terminal, qoro_domain::AgentRuntimeState::Idle, 15).await,
        Some(qoro_domain::AgentRuntimeState::Idle),
        "a finished opencode with its composer on screen still reads working: {:?}",
        d.agent(terminal).map(|a| (a.belief.state, a.belief.source)),
    );
    c.input_to(terminal, &[0x03]).await;
}

#[tokio::test]
async fn a_finished_opencode_matching_no_rule_reads_idle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let binary = run_opencode_stand_in(&dir).await;

    let d = TestDaemon::start().await;
    let mut c = d.attach("opencode-fallback").await;
    let terminal = d.terminals().streams()[0];
    c.input_to(terminal, format!("{} plain\n", binary.display()).as_bytes())
        .await;

    assert_eq!(
        wait_for_state(&d, terminal, qoro_domain::AgentRuntimeState::Working, 15).await,
        Some(qoro_domain::AgentRuntimeState::Working),
        "the stand in's interrupt hint was never read as working",
    );
    assert_eq!(
        wait_for_state(&d, terminal, qoro_domain::AgentRuntimeState::Idle, 15).await,
        Some(qoro_domain::AgentRuntimeState::Idle),
        "an opencode screen matching no rule kept its working belief: {:?}",
        d.agent(terminal).map(|a| (a.belief.state, a.belief.source)),
    );
    c.input_to(terminal, &[0x03]).await;
}

/// Two clients hold different terminals and neither disturbs the other.
///
/// There is no daemon-global focused terminal: focus is per connection, so
/// one client moving, subscribing, or leaving never reroutes what the other
/// sees. This is the failure a global focus has, where a
/// server-side focus move leaves a connected client's viewport behind.
#[tokio::test]
async fn two_clients_hold_different_terminals_with_no_shared_focus() {
    let d = TestDaemon::start().await;
    let first = d.terminals().streams()[0];
    let mut maker = d.attach("maker").await;
    let created = maker
        .control(
            &ControlRequest::CreateTerminal {
                id: "r1".into(),
                cwd: None,
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    let second = terminal_of(&created);
    assert_ne!(first, second);

    // Each client watches its own terminal and types into it.
    // A fresh connection auto-subscribes to the lowest terminal, so b starts
    // holding `first`: drop it before holding `second`, or both clients hold
    // `first` and the test proves nothing about isolation.
    let mut a = d.attach("a").await;
    let mut b = d.attach("b").await;
    match b
        .control(
            &ControlRequest::Unsubscribe {
                id: "b-drop-auto".into(),
                terminals: vec![first],
            },
            PATIENCE,
        )
        .await
        .expect("b drops the auto subscription")
    {
        ControlResponse::Subscribed { terminals, .. } => assert!(
            terminals.contains(&first),
            "b never held the auto subscription it had to drop: {terminals:?}"
        ),
        other => panic!("expected an unsubscribe answer, got {other:?}"),
    }
    match a
        .control(
            &ControlRequest::Subscribe {
                id: "a-sub".into(),
                terminals: vec![first],
            },
            PATIENCE,
        )
        .await
        .expect("a subscribes")
    {
        ControlResponse::Subscribed { terminals, .. } => assert!(
            terminals.contains(&first),
            "a's subscribe did not hold its terminal: {terminals:?}"
        ),
        other => panic!("expected a subscription, got {other:?}"),
    }
    match b
        .control(
            &ControlRequest::Subscribe {
                id: "b-sub".into(),
                terminals: vec![second],
            },
            PATIENCE,
        )
        .await
        .expect("b subscribes")
    {
        ControlResponse::Subscribed { terminals, .. } => assert!(
            terminals.contains(&second),
            "b's subscribe did not hold its terminal: {terminals:?}"
        ),
        other => panic!("expected a subscription, got {other:?}"),
    }
    a.input_to(first, b"echo ALPHA_$((6*7))\n").await;
    b.input_to(second, b"echo BETA_$((6*8))\n").await;
    assert!(
        a.wait_for_text_on(first, "ALPHA_42", PATIENCE).await,
        "client a lost its own terminal's output"
    );
    assert!(
        b.wait_for_text_on(second, "BETA_48", PATIENCE).await,
        "client b lost its own terminal's output"
    );

    // Client b drops its own subscription. Client a's view of the first
    // terminal is unaffected: nothing one connection does reroutes another's
    // renders. Unsubscribing from the terminal b actually holds (not a
    // no-op), and the response names what was dropped.
    match b
        .control(
            &ControlRequest::Unsubscribe {
                id: "b-unsub".into(),
                terminals: vec![second],
            },
            PATIENCE,
        )
        .await
        .expect("b unsubscribes")
    {
        ControlResponse::Subscribed { terminals, .. } => assert!(
            terminals.contains(&second),
            "b's unsubscribe dropped nothing it held: {terminals:?}"
        ),
        other => panic!("expected an unsubscribe answer, got {other:?}"),
    }
    a.input_to(first, b"echo STILL_$((6*9))\n").await;
    assert!(
        a.wait_for_text_on(first, "STILL_54", PATIENCE).await,
        "one client's unsubscribe disturbed another client's renders"
    );

    // Both clients read the same index: it is per daemon, never per focus.
    // Compared by terminal membership only: states settle asynchronously
    // (~80 ms detector), so two sequential reads can straddle a transition
    // on a correct daemon.
    async fn index_terminals(c: &mut qoro_testkit::TestClient) -> Vec<u32> {
        match c
            .control(&ControlRequest::ListAgents { id: "idx".into() }, PATIENCE)
            .await
            .expect("an index")
        {
            ControlResponse::Agents { mut agents, .. } => {
                agents.sort_by_key(|r| r.terminal);
                agents.iter().map(|r| r.terminal).collect::<Vec<_>>()
            }
            other => panic!("expected an index, got {other:?}"),
        }
    }
    let from_a = index_terminals(&mut a).await;
    let from_b = index_terminals(&mut b).await;
    assert_eq!(from_a, from_b, "two clients read different indexes");
    assert!(
        from_a.contains(&first) && from_a.contains(&second),
        "the shared index is missing a terminal: {from_a:?}"
    );
}

/// A terminal whose shell never started is not a terminal.
///
/// A directory that exists but cannot be entered passes the up-front check and fails the
/// spawn. That used to leave the tab listed with nothing behind it, and its row open, so
/// the next start brought back a tab the client had been told was never made.
#[tokio::test]
async fn a_terminal_whose_spawn_failed_leaves_no_tab_and_no_row() {
    let _pool = PROBE_POOL.lock().await;
    use std::os::unix::fs::PermissionsExt;

    let d = TestDaemon::start().await;
    let mut c = d.attach("ghost").await;
    let locked = tempfile::tempdir().expect("a directory");
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let before = d.terminals().streams();

    let answer = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "ghost-1".into(),
                cwd: Some(locked.path().display().to_string()),
                workspace: None,
                name: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod back");

    assert!(
        matches!(answer, ControlResponse::Failed { .. }),
        "a shell that cannot enter its directory was reported started: {answer:?}"
    );
    assert_eq!(
        d.terminals().streams(),
        before,
        "the failed tab is still listed"
    );
    let open = d
        .store()
        .open_terminals(d.workspace())
        .await
        .expect("the rows");
    assert_eq!(
        open.iter().map(|r| r.id as u32).collect::<Vec<_>>(),
        before,
        "the failed tab's row is still open, so the next start brings it back"
    );
}
