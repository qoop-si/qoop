//! A schedule's run history through the socket: each run has a row of its own, an edit keeps
//! the schedule's id, its runs and the report of a run that is going, a watch's start gets a
//! row, and a run ends `done` at its tab's turn end or `gone` when its tab closed first. A skip
//! cancels the next run only, and a complete schedule keeps its runs and stays off.
//!
//! The runs are made in the daemon's store directly, as the loop would: a run the daemon
//! started here would run this test binary as its `qoop schedule fire` child.

use std::time::{Duration, Instant};

use qoop_runtime::control::{
    ControlRequest, ControlResponse, ScheduleInfo, ScheduleOp, ScheduleRun,
};
use qoop_runtime::schedule::{Cron, next_slot, system_zone};
use qoop_store::schedule::ScheduleRow;
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    // Boxed: a control call holds a 64 KiB read buffer, and a test that awaits many of them
    // inline outgrows a debug build's test stack.
    Box::pin(c.control(&request, PATIENCE))
        .await
        .expect("an answer to the request")
}

fn store(d: &TestDaemon) -> qoop_store::Store {
    qoop_store::Store::open(&qoop_runtime::legacy::db_path(d.dir())).expect("the daemon's store")
}

/// A schedule that is not due: made now, so the daemon has no missed slot to claim.
fn row(name: &str) -> ScheduleRow {
    ScheduleRow {
        id: 0,
        name: name.into(),
        cron: Some("0 9 * * *".into()),
        at: None,
        agent: "claude".into(),
        cwd: "/tmp".into(),
        space: None,
        prompt: "look".into(),
        paused: false,
        created_at: qoop_runtime::store::now(),
        last_slot: None,
        last_at: None,
        last_result: None,
        last_terminal: None,
        watch: None,
        skip_until: None,
        done_at: None,
    }
}

fn write(n: &str, action: ScheduleOp) -> ControlRequest {
    ControlRequest::ScheduleWrite {
        id: format!("write-{n}"),
        terminal: None,
        token: None,
        action: Box::new(action),
    }
}

fn report(schedule: i64, slot: i64, terminal: Option<i64>, run: Option<i64>) -> ScheduleOp {
    ScheduleOp::Report {
        schedule,
        slot,
        result: "started".into(),
        terminal,
        seen: None,
        run,
    }
}

async fn runs(c: &mut TestClient, schedule: i64) -> Vec<ScheduleRun> {
    match ask(
        c,
        ControlRequest::ScheduleRuns {
            id: "runs".into(),
            schedule,
            before: None,
            limit: 100,
        },
    )
    .await
    {
        ControlResponse::ScheduleRuns { runs, .. } => runs,
        other => panic!("no runs: {other:?}"),
    }
}

/// The runs of `schedule` once `until` holds of them.
async fn runs_once(
    c: &mut TestClient,
    schedule: i64,
    until: impl Fn(&[ScheduleRun]) -> bool,
) -> Vec<ScheduleRun> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let listed = runs(c, schedule).await;
        if until(&listed) {
            return listed;
        }
        assert!(Instant::now() < deadline, "never so: {listed:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn written(c: &mut TestClient, n: &str, action: ScheduleOp) -> i64 {
    match ask(c, write(n, action)).await {
        ControlResponse::ScheduleWritten { schedule, .. } => schedule,
        other => panic!("not written: {other:?}"),
    }
}

#[tokio::test]
async fn run_now_is_refused_while_a_run_works_under_a_newer_skip() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("busy")).expect("a schedule");
    let now = qoop_runtime::store::now();
    let run = s
        .start_run(schedule, 0, now)
        .expect("a run")
        .expect("the schedule is there");
    // Its agent works in a tab that is open.
    let tab = i64::from(d.terminals().streams()[0]);
    written(&mut c, "1", report(schedule, 0, Some(tab), Some(run))).await;
    // The next slot was skipped while it works: a newer row, already ended.
    let skipped = s
        .claim_run(
            schedule,
            None,
            600,
            now + 1,
            "skipped: the last run still works",
        )
        .expect("a skip");
    assert!(skipped.is_some());
    match ask(&mut c, write("2", ScheduleOp::Run { schedule })).await {
        ControlResponse::Failed { reason, .. } => assert_eq!(reason, "still_running"),
        other => panic!("a second run beside a working one: {other:?}"),
    }
    let listed = runs(&mut c, schedule).await;
    assert_eq!(listed.len(), 2, "no row was added: {listed:?}");
}

#[tokio::test]
async fn a_run_whose_tab_closed_ends_gone() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("closed")).expect("a schedule");
    let run = s
        .start_run(schedule, 0, qoop_runtime::store::now())
        .expect("a run")
        .expect("the schedule is there");
    // A tab number no terminal has.
    written(&mut c, "1", report(schedule, 0, Some(4_000_000), Some(run))).await;
    let ended = runs_once(&mut c, schedule, |r| r[0].outcome.is_some()).await;
    assert_eq!(ended.len(), 1);
    assert_eq!(
        (
            ended[0].id,
            ended[0].result.as_str(),
            ended[0].outcome.as_deref(),
            ended[0].terminal
        ),
        (run, "started", Some("gone"), Some(4_000_000))
    );
    assert!(ended[0].ended_at.is_some() && ended[0].summary.is_none());
    // A schedule that is not there has no runs.
    assert!(runs(&mut c, schedule + 100).await.is_empty());
}

#[tokio::test]
async fn a_turn_end_in_the_runs_tab_ends_it_done() {
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut c = d.attach("schedules").await;
    let mut hook = d.attach("hook").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("turn")).expect("a schedule");
    let run = s
        .start_run(schedule, 0, qoop_runtime::store::now())
        .expect("a run")
        .expect("the schedule is there");
    written(
        &mut c,
        "1",
        report(schedule, 0, Some(i64::from(terminal)), Some(run)),
    )
    .await;
    assert_eq!(runs(&mut c, schedule).await[0].outcome, None, "still going");
    for (state, event) in [("working", "user"), ("idle", "done")] {
        let answer = ask(
            &mut hook,
            ControlRequest::ReportAgentState {
                from: None,
                id: format!("{state}-{event}"),
                terminal,
                token: token.clone(),
                state: state.into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: Some(event.into()),
                label: None,
                said: None,
            },
        )
        .await;
        assert!(
            !matches!(answer, ControlResponse::Failed { .. }),
            "{answer:?}"
        );
    }
    // A shell has no session file, so no summary: the agent's own is in `messages.rs`.
    let ended = runs_once(&mut c, schedule, |r| r[0].outcome.is_some()).await;
    assert_eq!(ended[0].outcome.as_deref(), Some("done"));
    assert_eq!(ended[0].terminal, Some(i64::from(terminal)));
}

#[tokio::test]
async fn an_edit_during_a_run_keeps_the_id_the_runs_and_that_runs_report() {
    let d = TestDaemon::start().await;
    let terminal = i64::from(d.terminals().streams()[0]);
    let mut c = d.attach("schedules").await;
    let dir = tempfile::tempdir().expect("tempdir");
    let schedule = written(
        &mut c,
        "add",
        ScheduleOp::Add {
            name: "nightly".into(),
            cron: Some("0 9 * * *".into()),
            at: None,
            watch: None,
            agent: None,
            cwd: dir.path().display().to_string(),
            space: None,
            prompt: "run the tests".into(),
        },
    )
    .await;
    // The loop's claim of a slot, with its run.
    let run = store(&d)
        .claim_run(schedule, None, 500, qoop_runtime::store::now(), "claimed")
        .expect("a claim")
        .expect("won");
    let change = |cron: &str| ScheduleOp::Change {
        schedule,
        name: None,
        cron: Some(cron.into()),
        at: None,
        watch: None,
        agent: None,
        cwd: None,
        space: None,
        prompt: Some("run every test".into()),
    };
    assert_eq!(
        written(&mut c, "edit", change("30 10 * * *")).await,
        schedule
    );
    let ControlResponse::Schedules { schedules, .. } =
        ask(&mut c, ControlRequest::ScheduleRead { id: "read".into() }).await
    else {
        panic!("no schedules");
    };
    let edited = schedules
        .iter()
        .find(|s| s.id == schedule)
        .expect("the same id");
    assert_eq!(
        (edited.cron.as_deref(), edited.prompt.as_str()),
        (Some("30 10 * * *"), "run every test")
    );
    assert!(
        edited.last_slot.is_some_and(|slot| slot > 500),
        "a new timing starts from now: {edited:?}"
    );
    // Refused as an add would be, and nothing changed.
    assert!(matches!(
        ask(&mut c, write("bad", change("not a cron"))).await,
        ControlResponse::Failed { .. }
    ));
    let missing = ScheduleOp::Change {
        schedule: schedule + 100,
        name: Some("other".into()),
        cron: None,
        at: None,
        watch: None,
        agent: None,
        cwd: None,
        space: None,
        prompt: None,
    };
    assert!(matches!(
        ask(&mut c, write("missing", missing)).await,
        ControlResponse::Failed { .. }
    ));
    // The run of the old slot reports after the edit: it lands on its row and on the
    // schedule's last run, since nothing started after it.
    written(
        &mut c,
        "2",
        report(schedule, 500, Some(terminal), Some(run)),
    )
    .await;
    let listed = runs(&mut c, schedule).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].id, listed[0].result.as_str(), listed[0].terminal),
        (run, "started", Some(terminal))
    );
    let ControlResponse::Schedules { schedules, .. } =
        ask(&mut c, ControlRequest::ScheduleRead { id: "read".into() }).await
    else {
        panic!("no schedules");
    };
    let after = schedules.iter().find(|s| s.id == schedule).expect("there");
    assert_eq!(
        (after.last_result.as_deref(), after.last_terminal),
        (Some("started"), Some(terminal))
    );
}

#[tokio::test]
async fn a_watch_that_started_an_agent_gets_a_row() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let s = store(&d);
    let watch = ScheduleRow {
        cron: None,
        watch: Some("echo one".into()),
        ..row("issues")
    };
    let schedule = s.add_schedule(&watch).expect("a watch");
    // Paused, so the daemon's look never runs its check (this test binary, as its child).
    assert!(s.pause_schedule(schedule).expect("paused"));
    written(&mut c, "1", report(schedule, 0, Some(9), None)).await;
    let listed = runs(&mut c, schedule).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].result.as_str(), listed[0].terminal),
        ("started", Some(9))
    );
}

/// The schedule as `ScheduleRead` lists it.
async fn listed(c: &mut TestClient, schedule: i64) -> ScheduleInfo {
    let ControlResponse::Schedules { schedules, .. } =
        ask(c, ControlRequest::ScheduleRead { id: "read".into() }).await
    else {
        panic!("no schedules");
    };
    schedules
        .into_iter()
        .find(|s| s.id == schedule)
        .expect("the schedule")
}

async fn refused(c: &mut TestClient, n: &str, action: ScheduleOp) -> String {
    match ask(c, write(n, action)).await {
        ControlResponse::Failed { reason, .. } => reason,
        other => panic!("not refused: {other:?}"),
    }
}

#[tokio::test]
async fn a_skip_moves_the_next_run_once_and_a_stale_slot_is_refused() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("daily")).expect("a schedule");
    let before = listed(&mut c, schedule).await;
    let first = before.next_at.expect("a next run");
    written(
        &mut c,
        "1",
        ScheduleOp::Skip {
            schedule,
            slot: first,
        },
    )
    .await;
    let daily = Cron::parse("0 9 * * *").expect("a cron");
    let second = next_slot(&daily, first, system_zone()).expect("the run after");
    let after = listed(&mut c, schedule).await;
    assert_eq!(
        (after.next_at, after.skip_until),
        (Some(second), Some(first))
    );
    assert_eq!(
        ScheduleInfo {
            next_at: before.next_at,
            skip_until: None,
            ..after
        },
        before,
        "nothing else changed"
    );
    assert!(runs(&mut c, schedule).await.is_empty());
    // The slot already cancelled, and one that is not next, are stale.
    for (n, slot) in [("2", first), ("3", second + 86_400)] {
        let why = refused(&mut c, n, ScheduleOp::Skip { schedule, slot }).await;
        assert_eq!(why, "stale_slot");
    }
    assert_eq!(listed(&mut c, schedule).await.next_at, Some(second));
    // A watch has no planned run, and an off schedule nothing planned.
    let watch = ScheduleRow {
        cron: None,
        watch: Some("echo one".into()),
        ..row("issues")
    };
    let watch = s.add_schedule(&watch).expect("a watch");
    // Paused, so the daemon's look never runs its check (this test binary, as its child).
    assert!(s.pause_schedule(watch).expect("paused"));
    let why = refused(
        &mut c,
        "4",
        ScheduleOp::Skip {
            schedule: watch,
            slot: first,
        },
    )
    .await;
    assert!(why.contains("watch"), "{why}");
    assert!(s.pause_schedule(schedule).expect("paused"));
    refused(
        &mut c,
        "5",
        ScheduleOp::Skip {
            schedule,
            slot: second,
        },
    )
    .await;
    // Off, the cancel is still said, so a client can name the run that turning it on brings.
    assert_eq!(listed(&mut c, schedule).await.skip_until, Some(first));
}

/// A cancelled run gets a row of its own, under its slot, that says so. A one-off's is written
/// with the cancel, and the one-off is done then: it reads as finished, not as on with nothing
/// planned.
#[tokio::test]
async fn a_cancelled_one_off_has_a_row_that_says_cancelled_and_is_done() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let at = qoop_runtime::store::now() + 3600;
    let once = ScheduleRow {
        cron: None,
        at: Some(at),
        ..row("once")
    };
    let schedule = store(&d).add_schedule(&once).expect("a one-off");
    written(&mut c, "1", ScheduleOp::Skip { schedule, slot: at }).await;
    let listed_runs = runs(&mut c, schedule).await;
    assert_eq!(
        (
            listed_runs.len(),
            listed_runs[0].slot,
            listed_runs[0].result.as_str(),
            listed_runs[0].outcome.as_deref()
        ),
        (1, at, "skipped: cancelled", Some("skipped"))
    );
    assert!(listed_runs[0].ended_at.is_some());
    let done = listed(&mut c, schedule).await;
    assert!(
        done.paused
            && done.done_at.is_some()
            && done.next_at.is_none()
            && done.skip_until.is_none()
            && done.last_result.as_deref() == Some("skipped: cancelled"),
        "{done:?}"
    );
    // Done: it is not cancelled twice, and does not come back on.
    refused(&mut c, "2", ScheduleOp::Skip { schedule, slot: at }).await;
    refused(&mut c, "3", ScheduleOp::Resume { schedule }).await;
}

#[tokio::test]
async fn complete_keeps_the_runs_and_a_complete_schedule_does_not_resume() {
    let d = TestDaemon::start().await;
    let mut c = d.attach("schedules").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("weekly")).expect("a schedule");
    let at = qoop_runtime::store::now();
    s.start_run(schedule, 0, at)
        .expect("a run")
        .expect("the schedule is there");
    written(&mut c, "1", ScheduleOp::Complete { schedule }).await;
    let done = listed(&mut c, schedule).await;
    assert!(done.paused && done.next_at.is_none(), "{done:?}");
    assert!(
        done.done_at
            .is_some_and(|t| (at..=qoop_runtime::store::now()).contains(&t)),
        "{done:?}"
    );
    assert_eq!(runs(&mut c, schedule).await.len(), 1);
    refused(&mut c, "2", ScheduleOp::Resume { schedule }).await;
    assert_eq!(listed(&mut c, schedule).await, done);
}

#[tokio::test]
async fn a_run_whose_agent_failed_ends_failed_with_no_turn_end() {
    // Claude's `StopFailure` (an API error, a usage limit) reports `failed` and no turn end;
    // the run must end anyway, or every later slot of the schedule is skipped as still open.
    let d = TestDaemon::start().await;
    let terminal = d.terminals().streams()[0];
    let token = d
        .terminals()
        .get(terminal)
        .expect("the terminal")
        .token
        .as_str()
        .to_string();
    let mut c = d.attach("schedules").await;
    let mut hook = d.attach("hook").await;
    let s = store(&d);
    let schedule = s.add_schedule(&row("failed")).expect("a schedule");
    let run = s
        .start_run(schedule, 0, qoop_runtime::store::now())
        .expect("a run")
        .expect("the schedule is there");
    written(
        &mut c,
        "1",
        report(schedule, 0, Some(i64::from(terminal)), Some(run)),
    )
    .await;
    for (state, event) in [("working", Some("user")), ("failed", None)] {
        let answer = ask(
            &mut hook,
            ControlRequest::ReportAgentState {
                from: None,
                id: state.to_string(),
                terminal,
                token: token.clone(),
                state: state.into(),
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                clock: 0,
                session: None,
                event: event.map(String::from),
                label: None,
                said: None,
            },
        )
        .await;
        assert!(
            !matches!(answer, ControlResponse::Failed { .. }),
            "{answer:?}"
        );
    }
    let ended = runs_once(&mut c, schedule, |r| r[0].outcome.is_some()).await;
    assert_eq!(ended[0].outcome.as_deref(), Some("failed"));
    assert_eq!(ended[0].terminal, Some(i64::from(terminal)));
}
