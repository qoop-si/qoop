//! A move whose store write misses its deadline is answered as failed, and the tab has to
//! stay where it is: through its sleep and its wake it comes back in its own space.
//!
//! Alone in its file on purpose: the stall below takes every thread of the process-wide
//! store pool for seconds, which would time out the store calls of any test beside it.

use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE)
        .await
        .expect("an answer to the request")
}

async fn new_space(c: &mut TestClient, name: &str) -> i64 {
    match ask(
        c,
        ControlRequest::CreateWorkspace {
            id: format!("space-{name}"),
            name: name.into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a created workspace, got {other:?}"),
    }
}

async fn new_tab(c: &mut TestClient, workspace: i64, id: &str) -> u32 {
    match ask(
        c,
        ControlRequest::CreateTerminal {
            id: id.into(),
            cwd: None,
            workspace: Some(workspace),
            name: None,
        },
    )
    .await
    {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

/// The ids a sleep or a wake changed. A fresh shell is refused a sleep while it starts, so
/// the sleep is asked again until it is not.
async fn change(c: &mut TestClient, workspace: i64, tab: u32, sleep: bool) -> Vec<u32> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let request = if sleep {
            ControlRequest::SleepWorkspace {
                id: "sleep".into(),
                workspace,
                terminal: Some(tab),
            }
        } else {
            ControlRequest::WakeWorkspace {
                id: "wake".into(),
                workspace: Some(workspace),
                terminal: Some(tab),
            }
        };
        match ask(c, request).await {
            ControlResponse::WorkspaceAsleep { changed, .. }
            | ControlResponse::WorkspaceAwake { changed, .. } => {
                return changed.expect("this daemon says what changed");
            }
            ControlResponse::Failed { reason, .. }
                if Instant::now() < deadline
                    && (reason.contains("starting") || reason.contains("not been read")) =>
            {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            other => panic!("not a sleep or a wake: {other:?}"),
        }
    }
}

/// The stall is another process holding the database's write lock, so the queued move runs
/// late, after the caller gave up.
#[tokio::test]
async fn a_move_that_missed_its_deadline_leaves_the_tab_in_its_own_space() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("late-move").await;
    let home = new_space(&mut c, "home").await;
    let away = new_space(&mut c, "away").await;
    let tab = new_tab(&mut c, home, "t1").await;

    let lock = rusqlite::Connection::open(qoop_runtime::legacy::db_path(d.dir())).expect("db");
    lock.execute_batch("BEGIN IMMEDIATE").expect("write lock");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(7));
        lock.execute_batch("COMMIT").expect("release");
    });
    // A write takes the store first and waits on the lock; the move queues behind it, runs
    // when that one gives up, and its own deadline has passed by the time the lock lets go.
    let first = d.store();
    tokio::spawn(async move {
        let _ = first.create_workspace("blocker".into()).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let moved = d
        .store()
        .move_terminals(vec![i64::from(tab)], away, None)
        .await;
    assert!(moved.is_err(), "the move should have missed its deadline");
    release.join().expect("lock released");

    assert_eq!(change(&mut c, home, tab, true).await, vec![tab]);
    assert_eq!(change(&mut c, home, tab, false).await, vec![tab]);
    let rows = match ask(&mut c, ControlRequest::ListWorkspaces { id: "list".into() }).await {
        ControlResponse::Workspaces { workspaces, .. } => workspaces,
        other => panic!("expected the spaces, got {other:?}"),
    };
    let count = |id: i64| rows.iter().find(|s| s.id == id).map(|s| s.terminals);
    assert_eq!(count(home), Some(1), "{rows:?}");
    assert_eq!(count(away), Some(0), "{rows:?}");
}
