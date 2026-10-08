//! A start with several tabs on a dead mount still answers a client in time.
//!
//! The socket is bound before restore and served only after it, so a client's handshake
//! deadline covers the whole restore. Each restored tab's directory is probed with a
//! deadline (ADR-011), and one probe after another made that deadline per tab: a handful
//! of tabs on a hung mount outlasted the client, which then failed to attach to a daemon
//! that was only busy.
//!
//! Its own test binary, because it fills the probe pool, which is process wide: in a shared
//! binary another test's probe would queue behind this one's stuck threads.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use qoop_runtime::{Daemon, blocking, paths};

/// Tabs recorded as open, each in a directory that will not be answered for.
const TABS: usize = 6;

/// A private directory a daemon will accept, kept alive by the returned handle.
fn runtime_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    paths::ensure_private_dir(dir.path()).expect("a private directory");
    dir
}

/// A hung mount is not something a test can install. What restore sees of one is a probe
/// that does not answer within its deadline, and a pool whose every thread is held by
/// probes that never return is exactly that for every directory at once: each restored
/// tab's probe waits the whole deadline and gets nothing. The sleeps release themselves,
/// so the test ends.
#[tokio::test(flavor = "multi_thread")]
async fn restore_with_every_directory_unanswered_waits_one_deadline_not_one_per_tab() {
    let dir = runtime_dir();
    let cwd = dir.path().join("work");
    std::fs::create_dir(&cwd).expect("mkdir");

    // Each tab in its own directory, so no prefix is degraded on the way and every probe
    // really waits.
    {
        let daemon = Daemon::bind(dir.path(), Path::new("/bin/sh"))
            .await
            .expect("the daemon binds");
        daemon
            .open_first_terminal()
            .await
            .expect("the first terminal");
        let terminals = daemon.terminals();
        let store = daemon.store();
        let workspace = daemon.workspace();
        for i in 0..TABS {
            let tab_dir = cwd.join(i.to_string());
            std::fs::create_dir(&tab_dir).expect("mkdir");
            store
                .create_terminal(
                    terminals.mint() as i64,
                    workspace,
                    tab_dir.display().to_string(),
                    "/bin/sh".into(),
                )
                .await
                .expect("a row");
        }
    }

    let stuck = Duration::from_secs(8);
    for _ in 0..blocking::PROBE_THREADS {
        tokio::spawn(blocking::probe(
            "/qoro-test-hung-mount",
            Duration::from_millis(100),
            move || std::thread::sleep(stuck),
        ));
    }
    // Every probe thread taken before the restore starts.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A shell whose startup moves it, as a login shell's rc file can: where it ends up is
    // not a directory the person chose for this tab.
    let moving_shell = dir.path().join("moving-shell");
    std::fs::write(&moving_shell, "#!/bin/sh\ncd /\nexec /bin/sh\n").expect("write");
    std::fs::set_permissions(&moving_shell, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let started = Instant::now();
    let daemon = Daemon::bind(dir.path(), &moving_shell)
        .await
        .expect("the daemon binds");
    let took = started.elapsed();
    let report = daemon.restore_report();

    // One probe deadline (two seconds) and the spawning, well inside the client's ten
    // second handshake. One deadline per tab would be fourteen.
    assert!(
        took < Duration::from_secs(5),
        "restoring {} tabs with unanswered directories took {took:?}: {report:?}",
        TABS + 1
    );
    // The first tab plus the ones recorded above, every one reopened in the
    // daemon's directory because nothing could say its own was there.
    assert_eq!(report.brought_back, TABS + 1, "{report:?}");
    assert_eq!(report.cwd_fallback, TABS + 1, "{report:?}");

    // And each row still names its own directory once the shells are up and have been read:
    // starting somewhere else because the directory was slow is not the person moving, and
    // written back, the next start would have lost the directory for good.
    let serving = tokio::spawn(async move { daemon.serve().await });
    tokio::time::sleep(Duration::from_secs(3)).await;
    serving.abort();
    let store = qoop_store::Store::open(&dir.path().join("qoro.db")).expect("open");
    let workspace = store.workspaces().expect("workspaces")[0].id;
    let rows = store.open_terminals(workspace).expect("rows");
    for i in 0..TABS {
        let asked = cwd.join(i.to_string()).display().to_string();
        assert!(
            rows.iter().any(|r| r.cwd == asked),
            "the row for {asked} lost its directory: {:?}",
            rows.iter().map(|r| &r.cwd).collect::<Vec<_>>()
        );
    }
}
