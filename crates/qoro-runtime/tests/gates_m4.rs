//! Milestone 4 gates: the durable log, and what depends on it.
//!
//! ADR-017. The rules the log makes are only worth anything if a projection really is a
//! fold over it, a retry really does return the original answer, and a terminal really does
//! keep running when the disk stops accepting writes.

use std::time::{Duration, Instant};

use qoro_events::{Body, Stream, WorkspaceView};
use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_testkit::{PATIENCE, TestDaemon};

/// G4.1: replay reconstructs every projection byte for byte, twice, under a frozen clock.
///
/// The clock is frozen by replaying a log whose timestamps are already written: a
/// projection that read the wall clock would produce a different answer on the second
/// replay, seconds later, and that is exactly what this catches. A projection that merely
/// replayed the same events at the same speed would pass whether or not it was pure.
#[tokio::test]
async fn g4_1_a_projection_replays_byte_for_byte_twice() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("projector").await;

    // A log with all four kinds in it.
    for i in 0..3 {
        c.control(
            &ControlRequest::CreateTerminal {
                id: format!("mk{i}"),
                cwd: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    }
    let events = d.events();
    events.observe(
        Stream::Terminal(1),
        Body::Observation {
            terminal: 1,
            state: "Working".into(),
            source: "Hook".into(),
            confidence: "High".into(),
            sampled_generation: 1,
            evidence: "a tool call started".into(),
            redacted: false,
        },
    );
    events.flush().await.expect("flush");

    let first = events.workspace_view().await.expect("project");
    // Long enough that anything reading the wall clock produces a different answer.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second = events.workspace_view().await.expect("project");

    assert_eq!(
        serde_json::to_string(&first).expect("encode"),
        serde_json::to_string(&second).expect("encode"),
        "two replays of the same log disagreed, so a projection is reading something \
         other than the log"
    );
    assert!(
        first.terminals.len() >= 3,
        "the log did not record the terminals that were created: {:?}",
        first.terminals
    );
    assert!(
        first.pending.is_empty(),
        "a command was acknowledged with no outcome in the log: {:?}",
        first.pending
    );
}

/// G4.6: the same command sent twice applies once and returns the original result, and the
/// replay arrives on a new connection after reconnecting.
///
/// The new connection is the whole point. A retry on the same connection is a retry the
/// daemon could answer from memory, which is not the case the gate is about: a client that
/// lost its connection mid-command has no idea whether the command landed.
#[tokio::test]
async fn g4_6_a_replayed_command_applies_once_and_returns_the_original_answer() {
    let d = TestDaemon::start().await;
    let before = d.terminals().len();

    let first_answer = {
        let mut c = d.attach("first").await;
        c.control(
            &ControlRequest::CreateTerminal {
                id: "same-key".into(),
                cwd: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response")
    };
    let ControlResponse::TerminalCreated { terminal, .. } = first_answer else {
        panic!("expected a created terminal, got {first_answer:?}");
    };
    assert_eq!(d.terminals().len(), before + 1);

    // A new connection, as if the first one had died before the answer arrived.
    let mut second = d.attach("second").await;
    let replayed = second
        .control(
            &ControlRequest::CreateTerminal {
                id: "same-key".into(),
                cwd: None,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    match replayed {
        ControlResponse::TerminalCreated {
            terminal: again, ..
        } => assert_eq!(
            again, terminal,
            "the retry created a second terminal instead of returning the first"
        ),
        other => panic!("the retry was not answered with the original result: {other:?}"),
    }
    assert_eq!(
        d.terminals().len(),
        before + 1,
        "the retry created another terminal"
    );
}

/// G4.4: with the store failing, terminals keep running and typing still works.
///
/// The rule is ADR-005's first and it is absolute. A terminal that waits for a disk is a
/// terminal whose child cannot exit, because an exiting child cannot complete until its tty
/// output is drained.
#[tokio::test]
async fn g4_4_a_failing_store_does_not_stop_a_terminal() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("degraded").await;

    c.input_to(terminal, b"echo BEFORE_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "BEFORE_42", PATIENCE).await);

    // Make the log unwritable, which is what a full or read only filesystem looks like.
    let db = d.dir().join("events.db");
    let mut perms = std::fs::metadata(&db).expect("metadata").permissions();
    <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::set_mode(&mut perms, 0o400);
    std::fs::set_permissions(&db, perms).expect("chmod");

    // A mutating command may fail, and must fail in words rather than hang.
    let answer = c
        .control(
            &ControlRequest::CreateTerminal {
                id: "while-degraded".into(),
                cwd: None,
            },
            PATIENCE,
        )
        .await;
    assert!(
        answer.is_some(),
        "a mutating command hung while the store was failing"
    );

    // And the terminal is untouched.
    c.input_to(terminal, b"echo AFTER_$((6*8))\n").await;
    assert!(
        c.wait_for_text_on(terminal, "AFTER_48", PATIENCE).await,
        "the terminal stopped when the store did"
    );
}

/// G4.3, the half that can be tested without killing a process: a command with no outcome
/// is what a restart reconciles, and a restart records a gap for the interval nobody saw.
#[tokio::test]
async fn g4_3_a_restart_records_a_gap_and_finds_what_it_has_to_reconcile() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(
        dir.path(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("chmod");
    qoro_runtime::paths::ensure_private_dir(dir.path()).expect("private dir");

    // A first daemon writes an intent with no outcome, as a crash mid-command would leave.
    {
        let daemon = {
            let _binding = qoro_testkit::BINDING.lock().await;
            qoro_runtime::Daemon::bind(dir.path(), std::path::Path::new("/bin/sh"))
                .await
                .expect("bind")
        };
        let events = daemon.events();
        events
            .append_durable(vec![(
                Stream::Command("orphan".into()),
                Body::Command {
                    idempotency_key: "orphan".into(),
                    operation: "CreateTerminal".into(),
                    detail: serde_json::json!({ "cwd": "/tmp" }),
                },
            )])
            .await
            .expect("append");
        assert_eq!(
            events.unreconciled().await.expect("scan"),
            vec!["orphan".to_string()]
        );
    }

    // A second daemon over the same directory.
    let daemon = {
        let _binding = qoro_testkit::BINDING.lock().await;
        qoro_runtime::Daemon::bind(dir.path(), std::path::Path::new("/bin/sh"))
            .await
            .expect("rebind")
    };
    let view: WorkspaceView = daemon.events().project().await.expect("project");
    assert!(
        view.gaps.iter().any(|g| g.reason.contains("restarted")),
        "a restart recorded no gap, so a reader would assume continuity across it: {:?}",
        view.gaps
    );
    assert_eq!(
        daemon.events().unreconciled().await.expect("scan"),
        vec!["orphan".to_string()],
        "the restart lost the intent it has to reconcile"
    );
}

/// G4.2, at the level the daemon enforces it: an observation never waits, and what is
/// dropped under pressure is covered by a gap whose count matches.
#[tokio::test]
async fn g4_2_observations_are_lossy_and_the_loss_is_recorded() {
    let d = TestDaemon::start().await;
    let events = d.events();
    let over = 120usize;
    let capacity = qoro_runtime::eventlog::OBSERVATION_CAPACITY;

    let start = Instant::now();
    for i in 0..(capacity + over) {
        events.observe(
            Stream::Terminal(1),
            Body::Observation {
                terminal: 1,
                state: "Working".into(),
                source: "ScreenHeuristic".into(),
                confidence: "Low".into(),
                sampled_generation: 1,
                evidence: format!("line {i}"),
                redacted: false,
            },
        );
    }
    // Never waits. The whole point of the buffer is that this path cannot be slowed by a
    // disk, so the loop above has to finish in the time a memory write takes.
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "recording observations took {:?}, so it is waiting on something",
        start.elapsed()
    );

    events.flush().await.expect("flush");
    let view = events.workspace_view().await.expect("project");
    let recorded: u64 = view.gaps.iter().map(|g| g.dropped).sum();
    assert_eq!(
        recorded, over as u64,
        "the gap does not account for what was dropped"
    );
}

/// G8.3: cleanup with a process holding a file inside the tree.
///
/// The ordinary case, not an edge case: an editor, a language server, a build watcher.
/// Removing the tree under one produces a half removed tree and a confused process, so
/// cleanup terminates the terminal's processes through the ADR-014 machinery first and only
/// then removes anything.
#[tokio::test]
async fn g8_3_cleanup_terminates_what_holds_the_tree_before_removing_it() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("mover").await;

    let tree = d.dir().join("worktree");
    std::fs::create_dir(&tree).expect("mkdir");
    std::fs::write(tree.join("held.txt"), "x").expect("write");
    let pid_file = tree.join("holder.pid");

    // A process holding a file inside the tree, in its own process group so that reaching
    // it needs the containment set rather than the shell's group.
    let script = d.dir().join("holder.sh");
    std::fs::write(
        &script,
        format!(
            "echo $$ > {}\nexec sh -c 'while true; do sleep 0.05; done' < {}\n",
            pid_file.display(),
            tree.join("held.txt").display()
        ),
    )
    .expect("write");
    c.input_to(
        terminal,
        format!("nohup sh {} >/dev/null 2>&1 &\n", script.display()).as_bytes(),
    )
    .await;
    c.input_to(terminal, b"echo STARTED_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "STARTED_42", PATIENCE).await);

    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && !pid_file.exists() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let holder: i32 = std::fs::read_to_string(&pid_file)
        .expect("the holder wrote its pid")
        .trim()
        .parse()
        .expect("a numeric pid");
    qoro_testkit::track_pid(holder);
    // SAFETY: signal 0 only probes for existence.
    assert!(
        unsafe { libc::kill(holder, 0) } == 0,
        "the holder never ran"
    );

    // Closing the terminal is what runs teardown, and teardown is what has to reach it.
    c.input_to(terminal, b"exit\n").await;

    let deadline = Instant::now() + Duration::from_secs(8);
    // SAFETY: signal 0 only probes for existence.
    while Instant::now() < deadline && unsafe { libc::kill(holder, 0) } == 0 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // SAFETY: signal 0 only probes for existence.
    assert!(
        unsafe { libc::kill(holder, 0) } != 0,
        "the process holding a file in the tree survived teardown, so removing the tree \
         would leave it half removed under a confused process"
    );

    // And only now is the tree removable.
    std::fs::remove_dir_all(&tree).expect("the tree is removable once nothing holds it");
    assert!(!tree.exists());
}

/// G4.5: a waiter still resolves across an injected buffer overflow.
///
/// The failure this rules out is the one that makes a waiter useless exactly when it is
/// needed: an agent becomes blocked during a burst, the observation that says so is the one
/// dropped under pressure, and a waiter that only folds events forward waits for an event
/// that no longer exists.
///
/// The gap is what obliges it to re-read state instead (ADR-017), which is why a gap is an
/// event rather than a counter.
#[tokio::test(flavor = "multi_thread")]
async fn g4_5_a_waiter_resolves_across_an_injected_buffer_overflow() {
    let d = TestDaemon::start().await;
    let events = d.events();
    let capacity = qoro_runtime::eventlog::OBSERVATION_CAPACITY;

    let waiting = {
        let events = events.clone();
        tokio::spawn(async move {
            events
                .wait_for_state(
                    1,
                    |summary| summary.state == "Blocked",
                    Duration::from_secs(20),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The buffer is filled first, and only then is the observation the waiter wants
    // submitted, because the buffer drops what arrives while it is full rather than what is
    // oldest. The first version of this had it the other way round and submitted the
    // wanted observation first, on the stated assumption that an early entry is displaced.
    // It is not: an early entry is kept, and the test passed only because the burst's own
    // noise usually overwrote the state afterwards. Measured, it failed about one run in
    // fifteen, and when it passed it was not testing a waiter across a lost event.
    //
    // Nothing is awaited between filling and submitting, so the writer cannot drain the
    // buffer in between, and the assertion below says whether the arrangement held.
    // Attempts, because the writer drains on another thread and can empty the buffer
    // between the fill and the submission. Measured, that happened in two runs of fifteen
    // with one attempt.
    let mut arranged = false;
    for _ in 0..10 {
        for i in 0..(capacity * 2) {
            events.observe(
                Stream::Terminal(1),
                Body::Observation {
                    terminal: 1,
                    state: "Working".into(),
                    source: "ScreenHeuristic".into(),
                    confidence: "Low".into(),
                    sampled_generation: 1,
                    evidence: format!("noise {i}"),
                    redacted: false,
                },
            );
        }
        let before = events.dropped_observations();
        events.observe(
            Stream::Terminal(1),
            Body::Observation {
                terminal: 1,
                state: "Blocked".into(),
                source: "Hook".into(),
                confidence: "High".into(),
                sampled_generation: 1,
                evidence: "waiting for an answer".into(),
                redacted: false,
            },
        );
        if events.dropped_observations() > before {
            arranged = true;
            break;
        }
    }
    assert!(
        arranged,
        "the observation the waiter wants was accepted rather than dropped, so nothing here \
         is being tested"
    );
    events.flush().await.expect("flush");

    // The observation the waiter wanted really was dropped, or this proves nothing.
    let view = events.workspace_view().await.expect("project");
    assert!(
        !view.gaps.is_empty(),
        "the buffer did not overflow, so there is no gap to resolve across"
    );
    assert_ne!(
        view.agent_state.get(&1).map(|s| s.state.as_str()),
        Some("Blocked"),
        "the observation survived the burst, so the waiter is not being tested"
    );
    assert!(
        !waiting.is_finished(),
        "the waiter resolved before the state was ever blocked"
    );

    // Now the state really is blocked, and the waiter has to notice by re-reading rather
    // than by folding an event it already missed.
    events.observe(
        Stream::Terminal(1),
        Body::Observation {
            terminal: 1,
            state: "Blocked".into(),
            source: "Hook".into(),
            confidence: "High".into(),
            sampled_generation: 1,
            evidence: "still waiting".into(),
            redacted: false,
        },
    );
    events.flush().await.expect("flush");

    let resolved = tokio::time::timeout(Duration::from_secs(25), waiting)
        .await
        .expect("the waiter did not finish at all")
        .expect("the waiter task panicked")
        .expect("the waiter failed");
    assert!(
        resolved,
        "the waiter timed out across a gap, so it was folding forward rather than re-reading"
    );
}

/// And a waiter that is never satisfied gives up rather than hanging.
#[tokio::test(flavor = "multi_thread")]
async fn a_waiter_that_is_never_satisfied_times_out() {
    let d = TestDaemon::start().await;
    let resolved = d
        .events()
        .wait_for_state(
            99,
            |summary| summary.state == "Blocked",
            Duration::from_millis(600),
        )
        .await
        .expect("no error");
    assert!(!resolved);
}
