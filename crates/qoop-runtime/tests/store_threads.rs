//! A hung database holds the store pool's threads and no more (ADR-011, gate G2.1).
//!
//! Its own test binary, because the assertion is a count of this process's threads and a
//! neighbouring test that spawns a terminal would move it.

use std::time::Duration;

use qoop_runtime::blocking::STORE_THREADS;
use qoop_runtime::store::StoreHandle;

/// This process's thread count, from the kernel.
fn threads() -> usize {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain C data, for which all zero is a valid value.
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: info is a live, correctly sized buffer for PROC_PIDTASKINFO.
        let rc = unsafe {
            libc::proc_pidinfo(
                std::process::id() as i32,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        assert_eq!(rc, size, "the kernel did not describe this process");
        info.pti_threadnum as usize
    }
    #[cfg(not(target_os = "macos"))]
    {
        let status = std::fs::read_to_string("/proc/self/status").expect("status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .and_then(|n| n.trim().parse().ok())
            .expect("a thread count")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_database_holds_only_the_store_pool() {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("qoop.db");
    let handle = StoreHandle::new(qoop_store::Store::open(&path).expect("open"));
    handle.create_workspace("w".into()).await.expect("a write");

    // Another writer holds the database, so every write waits in SQLite's busy handler
    // with the store lock held, and every call behind it waits for that lock.
    let wedge = rusqlite::Connection::open(&path).expect("a second connection");
    wedge.execute_batch("BEGIN EXCLUSIVE").expect("lock");

    let before = threads();
    let calls: Vec<_> = (0..20)
        .map(|i| {
            let handle = handle.clone();
            tokio::spawn(async move { handle.create_workspace(format!("w{i}")).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let during = threads();

    // Let go, so the calls still queued finish rather than holding the runtime open.
    wedge.execute_batch("ROLLBACK").expect("unlock");
    for call in calls {
        let _ = call.await;
    }

    assert!(
        during <= before + STORE_THREADS,
        "a hung database grew the process from {before} to {during} threads, \
         past the store pool's {STORE_THREADS}"
    );
}
