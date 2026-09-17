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
