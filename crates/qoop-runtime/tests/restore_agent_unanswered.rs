//! A busy start is not a missing folder, and an unanswered one is not resumed in.
//!
//! An agent whose folder is not there is not resumed, so what counts as "not there" matters
//! twice. A probe pool full of stuck probes makes every folder look unanswered for a moment:
//! an agent with a perfectly good folder must still come back, or one hung mount would end
//! every conversation at the next start. And a folder that really does not answer must not
//! have its agent resumed in some other folder.
//!
//! Its own test binary, because it fills the probe pool, which is process wide.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use qoop_runtime::{Daemon, blocking, paths};

fn runtime_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    paths::ensure_private_dir(dir.path()).expect("a private directory");
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_pool_does_not_end_a_conversation_and_a_hung_folder_is_not_resumed_in() {
    let dir = runtime_dir();
    let held = tempfile::tempdir().expect("a directory");
    let base = std::fs::canonicalize(held.path()).expect("a real path");
    let fine = base.join("fine");
    std::fs::create_dir(&fine).expect("mkdir");
    let overrides = base.join("overrides");
    std::fs::create_dir(&overrides).expect("mkdir");
    for agent in ["busy-fine-xyz", "busy-hung-xyz"] {
        std::fs::write(
            overrides.join(format!("{agent}.toml")),
            format!(
                "id = \"{agent}\"\nversion = \"test\"\n[resume]\nlatest = [\"/usr/bin/touch\", \"{}\"]\n",
                base.join(format!("{agent}.resumed")).display()
            ),
        )
        .expect("an override");
    }
    let outcome = qoop_detect::reload_overrides(&overrides);
    assert!(outcome.errors.is_empty(), "{outcome:?}");

    let (fine_tab, hung_tab) = {
        let daemon = Daemon::bind(dir.path(), Path::new("/bin/sh"))
            .await
            .expect("the daemon binds");
        let (terminals, store, workspace) =
            (daemon.terminals(), daemon.store(), daemon.workspace());
        let mut ids = Vec::new();
        for (cwd, agent) in [
            (fine.display().to_string(), "busy-fine-xyz"),
            ("/qoop-test-hung-mount/work".to_string(), "busy-hung-xyz"),
        ] {
            let id = terminals.mint();
            store
                .create_terminal(id as i64, workspace, cwd, "/bin/sh".into())
                .await
                .expect("a row");
            store
                .set_agent(id as i64, Some(agent.into()), Some("session-x".into()))
                .await
                .expect("an agent");
            ids.push(id);
        }
        (ids[0], ids[1])
    };

    // Every probe thread taken before the restore starts, under one prefix, as a hung
    // mount holds them.
    for _ in 0..blocking::PROBE_THREADS {
        tokio::spawn(blocking::probe(
            "/qoop-test-hung-mount",
            Duration::from_millis(100),
            move || std::thread::sleep(Duration::from_secs(8)),
        ));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let started = Instant::now();
    let daemon = Daemon::bind(dir.path(), Path::new("/bin/sh"))
        .await
        .expect("the daemon binds");
    let took = started.elapsed();
    let report = daemon.restore_report();
    assert!(took < Duration::from_secs(5), "{took:?}: {report:?}");

    let fine_marker = base.join("busy-fine-xyz.resumed");
    let deadline = Instant::now() + Duration::from_secs(8);
    while !fine_marker.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        fine_marker.exists(),
        "a busy start ended a conversation whose folder is fine: {report:?}"
    );
    // The control has run, so what typed it had time to type the other.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !base.join("busy-hung-xyz.resumed").exists(),
        "an agent whose folder did not answer was resumed"
    );

    assert_eq!(report.agents_resumed, 1, "{report:?}");
    assert_eq!(report.agents_folder_gone, 1, "{report:?}");
    let rows = daemon
        .store()
        .open_terminals(daemon.workspace())
        .await
        .expect("rows");
    let row = |id: u32| rows.iter().find(|r| r.id == id as i64).expect("a row");
    assert_eq!(row(fine_tab).agent.as_deref(), Some("busy-fine-xyz"));
    assert_eq!(row(hung_tab).agent, None);
    assert_eq!(row(hung_tab).agent_session.as_deref(), Some("session-x"));
}
