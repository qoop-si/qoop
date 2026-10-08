//! One state directory, one daemon.
//!
//! The database and the history live outside the runtime directory now (ADR-005), so two
//! runtime directories can resolve to one state directory: an ssh relay with no
//! `XDG_RUNTIME_DIR` beside the desktop's daemon. Two daemons on one database would sweep
//! each other's history, so the second is refused, and refused before it binds a socket
//! that clients would wait on.

use std::os::unix::fs::PermissionsExt;

fn private() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

#[tokio::test]
async fn a_second_daemon_on_the_same_state_is_refused_before_it_binds() {
    let (first_dir, second_dir, state) = (private(), private(), private());
    let shell = std::path::Path::new("/bin/sh");
    let _first = qoop_runtime::Daemon::bind_in(first_dir.path(), state.path(), shell)
        .await
        .expect("the first daemon starts");
    let second = qoop_runtime::Daemon::bind_in(second_dir.path(), state.path(), shell).await;
    let Err(refused) = second else {
        panic!("a second daemon opened the same state");
    };
    assert!(
        refused.to_string().contains("another qoop daemon is using"),
        "refused without saying why: {refused}"
    );
    assert!(
        !second_dir.path().join("qoopd.sock").exists(),
        "the refused daemon bound a socket clients would wait on"
    );
}
