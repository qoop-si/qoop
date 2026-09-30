//! Milestone 4 gates: the idempotency ledger, and what depends on it.
//!
//! ADR-017. The daemon no longer writes an event log; what it keeps is the one part of it
//! it ever read, the ledger that makes a retry return the original answer. It lives in the
//! metadata store, and these are its gates. What went with the log and is covered nowhere
//! now is listed in ADR-005, "The log is not written".

use std::time::{Duration, Instant};

use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_testkit::{PATIENCE, TestDaemon};

fn create(id: &str) -> ControlRequest {
    ControlRequest::CreateTerminal {
        id: id.into(),
        cwd: None,
        workspace: None,
        name: None,
    }
}

/// The new connection is the whole point. A retry on the same connection is a retry the
/// daemon could answer from memory, which is not the case the gate is about: a client that
/// lost its connection mid-command has no idea whether the command landed.
#[tokio::test]
async fn g4_6_a_replayed_command_applies_once_and_returns_the_original_answer() {
    let d = TestDaemon::start().await;
    let before = d.terminals().len();

    let first_answer = {
        let mut c = d.attach("first").await;
        c.control(&create("same-key"), PATIENCE)
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
        .control(&create("same-key"), PATIENCE)
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

/// A close is a mutating command too, and it goes through the same ledger.
///
/// G4.6 covers a create. A close is the one where a second attempt has a visible cost, so
/// it is worth stating separately: the retry answers from the ledger rather than reporting
/// the terminal the first close already removed.
///
/// The wait before the retry is the whole test. A close answers before its teardown
/// finishes, and the registry entry survives until the terminal task deregisters itself,
/// so a retry sent in that window is answered by a second real close whether or not the
/// ledger exists. Measured: a close with a key the ledger has never seen answers
/// `TerminalClosed` for about 300ms after the first one. Only once the terminal is gone
/// does a missing ledger produce `Failed { no terminal N }` instead.
#[tokio::test]
async fn g4_6_a_replayed_close_returns_the_original_answer() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];

    let first = {
        let mut c = d.attach("closer").await;
        c.control(
            &ControlRequest::CloseTerminal {
                id: "close-key".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
        .expect("a response")
    };
    assert!(
        matches!(first, ControlResponse::TerminalClosed { .. }),
        "the first close was refused: {first:?}"
    );

    // The teardown has to finish, or the retry lands in the window where a second real
    // close would answer the same way and this gate proves nothing.
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && !d.terminals().is_empty() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        d.terminals().is_empty(),
        "the teardown never finished, so a retry cannot be told apart from a second close"
    );

    // A new connection, as if the answer never arrived. There is no terminal left to close,
    // so this is answered from the ledger or not at all.
    let mut second = d.attach("closer-again").await;
    let replayed = second
        .control(
            &ControlRequest::CloseTerminal {
                id: "close-key".into(),
                terminal,
            },
            PATIENCE,
        )
        .await
        .expect("a response");
    assert!(
        matches!(replayed, ControlResponse::TerminalClosed { terminal: t, .. } if t == terminal),
        "the retry was not answered from the ledger: {replayed:?}"
    );
}

/// The answer is on disk, not in the daemon's memory.
///
/// The stronger claim, that it survives the daemon, is checked end to end against the
/// packaged binary in `qoro-cli`'s `ledger.rs`: that one stops the first daemon and starts a
/// second over the same directory. What this gate adds is the placement, which is the part
/// a person can get wrong without noticing for weeks: the row is in `qoro.db`, next to the
/// terminal rows, because that is the file the next daemon opens.
#[tokio::test]
async fn g4_6_a_replayed_command_is_answered_from_the_store_not_from_memory() {
    let d = TestDaemon::start().await;
    let before = d.terminals().len();

    {
        let mut c = d.attach("writer").await;
        c.control(&create("in-the-store"), PATIENCE)
            .await
            .expect("a response");
    }
    assert_eq!(d.terminals().len(), before + 1);

    // The same store file, opened the way `qoro daemon` opens it.
    let store = qoro_store::Store::open(&d.dir().join("qoro.db")).expect("open the store");
    let applied = store.applied("in-the-store").expect("lookup");
    assert!(
        applied.is_some(),
        "the answer was not written where the next daemon will look"
    );
}

/// G4.4: with the store failing, terminals keep running and typing still works.
///
/// The rule is ADR-005's first and it is absolute. A terminal that waits for a disk is a
/// terminal whose child cannot exit, because an exiting child cannot complete until its tty
/// output is drained.
///
/// The ledger table is dropped from a second connection, so every read and write of it
/// fails for real. A chmod would not do: the daemon holds its descriptor open.
#[tokio::test]
async fn g4_4_a_failing_store_does_not_stop_a_terminal() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let mut c = d.attach("degraded").await;

    c.input_to(terminal, b"echo BEFORE_$((6*7))\n").await;
    assert!(c.wait_for_text_on(terminal, "BEFORE_42", PATIENCE).await);

    rusqlite::Connection::open(d.dir().join("qoro.db"))
        .expect("a second connection")
        .execute_batch("DROP TABLE applied")
        .expect("break the ledger");

    // A create whose answer cannot be written is refused in words, and leaves no tab
    // behind: a client told `Failed` must not have a shell running.
    let before = d.terminals().len();
    let rows = || async {
        d.store()
            .open_terminals(d.workspace())
            .await
            .expect("the terminal table still reads")
            .len()
    };
    let rows_before = rows().await;
    let answer = c.control(&create("while-degraded"), PATIENCE).await;
    assert!(
        matches!(answer, Some(ControlResponse::Failed { .. })),
        "a create with no ledger was answered as: {answer:?}"
    );
    assert_eq!(d.terminals().len(), before, "a refused create left a tab");
    assert_eq!(
        rows().await,
        rows_before,
        "a refused create left a row the next start would open"
    );

    // The terminal is untouched.
    c.input_to(terminal, b"echo AFTER_$((6*8))\n").await;
    assert!(
        c.wait_for_text_on(terminal, "AFTER_48", PATIENCE).await,
        "the terminal stopped when the store did"
    );

    // And a close still closes: a ledger that cannot be read is "not applied", not a refusal.
    let closed = c
        .control(
            &ControlRequest::CloseTerminal {
                id: "close-while-degraded".into(),
                terminal,
            },
            PATIENCE,
        )
        .await;
    assert!(
        matches!(closed, Some(ControlResponse::TerminalClosed { .. })),
        "a close with no ledger was answered as: {closed:?}"
    );
}

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
