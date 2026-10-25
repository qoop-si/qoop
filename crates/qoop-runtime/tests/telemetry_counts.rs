//! Usage counts are taken where a thing happens, and once (ADR-023): a space created and a tab
//! asked for count, a tab a wake brings back does not, and a card put away counts the first
//! time only. Driven through the socket against a real daemon.

use std::time::{Duration, Instant};

use qoop_runtime::control::{AskOp, ControlRequest, ControlResponse, ScheduleOp};
use qoop_runtime::telemetry::{Standing, Telemetry, day_of};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE)
        .await
        .expect("an answer to the request")
}

/// Asked again while the fresh shell is still starting, which refuses a sleep.
async fn sleep_tab(c: &mut TestClient, workspace: i64, terminal: u32) {
    let deadline = Instant::now() + PATIENCE;
    for n in 0.. {
        let answer = ask(
            c,
            ControlRequest::SleepWorkspace {
                id: format!("sleep-{n}"),
                workspace,
                terminal: Some(terminal),
            },
        )
        .await;
        let starting = matches!(&answer, ControlResponse::Failed { reason, .. }
            if reason.contains("starting") || reason.contains("not been read"));
        if !starting {
            assert!(
                matches!(
                    answer,
                    ControlResponse::WorkspaceAsleep { terminals: 1, .. }
                ),
                "{answer:?}"
            );
            return;
        }
        assert!(Instant::now() < deadline, "the tab never settled");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn dismiss(n: u32) -> ControlRequest {
    ControlRequest::AskWrite {
        id: format!("dismiss-{n}"),
        terminal: None,
        token: None,
        action: AskOp::Dismiss { card: "E1".into() },
    }
}

#[tokio::test]
async fn each_thing_counts_where_it_happens_and_once() {
    let d = TestDaemon::start().await;
    let files = tempfile::tempdir().expect("tempdir");
    std::fs::write(files.path().join("noticed"), "seen\n").expect("notice");
    let t = Telemetry::with_files(
        d.store(),
        "http://127.0.0.1:9/api/telemetry".into(),
        files.path().join("config.toml"),
        files.path().join("noticed"),
    );
    assert_eq!(t.check().await, Standing::On);
    t.activate();
    let mut c = d.attach("telemetry").await;

    let space = match ask(
        &mut c,
        ControlRequest::CreateWorkspace {
            id: "space".into(),
            name: "work".into(),
        },
    )
    .await
    {
        ControlResponse::WorkspaceCreated { workspace, .. } => workspace,
        other => panic!("expected a space, got {other:?}"),
    };
    let tab = match ask(
        &mut c,
        ControlRequest::CreateTerminal {
            id: "tab".into(),
            cwd: None,
            workspace: Some(space),
            name: None,
        },
    )
    .await
    {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a tab, got {other:?}"),
    };
    // A wake opens the tab again: a restore, not a new terminal.
    sleep_tab(&mut c, space, tab).await;
    let woke = ask(
        &mut c,
        ControlRequest::WakeWorkspace {
            id: "wake".into(),
            workspace: Some(space),
            terminal: Some(tab),
        },
    )
    .await;
    // A wake that brought nothing back would prove nothing about restores.
    assert!(
        matches!(woke, ControlResponse::WorkspaceAwake { terminals: 1, .. }),
        "{woke:?}"
    );

    // A card the owner puts away twice: closed once, then already ended.
    let db = qoop_runtime::legacy::db_path(d.dir());
    let store = qoop_store::Store::open(&db).expect("the daemon's store");
    store
        .add_ask("E1", None, None, None, "{}", 1)
        .expect("a card");
    for n in 0..2 {
        let answer = ask(&mut c, dismiss(n)).await;
        assert!(
            matches!(answer, ControlResponse::AskWritten { .. }),
            "{answer:?}"
        );
    }

    // A schedule's run that started an agent counts; one that reports anything else, or for a
    // slot the row no longer holds, does not.
    let schedule = store
        .add_schedule(&qoop_store::schedule::ScheduleRow {
            id: 0,
            name: "daily".into(),
            cron: Some("0 9 * * *".into()),
            at: None,
            agent: "claude".into(),
            cwd: "/tmp".into(),
            space: None,
            prompt: "look".into(),
            paused: false,
            // Made now, so the daemon has no missed slot to catch up on and claim.
            created_at: qoop_runtime::store::now(),
            last_slot: None,
            last_at: None,
            last_result: None,
            last_terminal: None,
            watch: None,
        })
        .expect("a schedule");
    // The slot the row holds now: the daemon's own look may have claimed one meanwhile.
    let held = store
        .schedule(schedule)
        .expect("read")
        .and_then(|row| row.last_slot)
        .unwrap_or(0);
    for (n, slot, result) in [
        (0, held, "skipped: busy"),
        (1, held, "started"),
        (2, held + 1_000_000, "started"),
    ] {
        let answer = ask(
            &mut c,
            ControlRequest::ScheduleWrite {
                id: format!("report-{n}"),
                terminal: None,
                token: None,
                action: Box::new(ScheduleOp::Report {
                    schedule,
                    slot,
                    result: result.into(),
                    terminal: None,
                    seen: None,
                }),
            },
        )
        .await;
        assert!(
            matches!(answer, ControlResponse::ScheduleWritten { .. }),
            "{answer:?}"
        );
    }

    // The counts are written behind the answers: read until they are there, then once more
    // after a pause, so a late extra count would show.
    let day = day_of(qoop_runtime::store::now());
    let deadline = Instant::now() + PATIENCE;
    let counts = |p: &serde_json::Value| {
        (
            p["spaces_created"].as_u64(),
            p["terms_opened"].as_u64(),
            p["q_dismissed"].as_u64(),
            p["schedule_runs"].as_u64(),
        )
    };
    loop {
        // No payload until the first count has written the id.
        let now = t.payload(&day).await.map(|p| counts(&p));
        if now == Some((Some(1), Some(1), Some(1), Some(1))) {
            break;
        }
        assert!(Instant::now() < deadline, "counted {now:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let p = t.payload(&day).await.expect("today");
    assert_eq!(counts(&p), (Some(1), Some(1), Some(1), Some(1)));
}
