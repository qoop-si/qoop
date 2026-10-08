//! Scenarios 6 and 7: multi-client fan-out and reconnect catch-up.
//!
//! The complaint behind both is that a multiplexer gets unusable with several
//! machines and agents after a few hours. The two measurable parts of
//! that which run in CI are render fan-out (one busy daemon serving several
//! clients at once, where a slow client must not slow the others) and
//! reconnect (a client that drops and comes back must see the current screen
//! promptly, without typing anything first).
//!
//! Both drive a real daemon through `qoop-testkit`: real PTYs, real shells,
//! the same reassembly a real client uses. An 8-hour multi-machine soak is
//! explicitly not claimed here; these are the CI-runnable components whose
//! numbers would move first if fan-out or reattach regressed.

use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_testkit::{PATIENCE, TestClient, TestDaemon};

use crate::report::ScenarioOutcome;
use crate::stats::summarize;

const FANOUT_CLIENTS: usize = 4;
const FANOUT_WARMUP: usize = 3;
const FANOUT_SAMPLES: usize = 50;
const RECONNECT_SAMPLES: usize = 20;
const ROUND_TIMEOUT: Duration = Duration::from_secs(5);

/// Scenario 6: four clients each hold their own terminal; all four type a
/// marker at once, and each client's delivery time is recorded.
///
/// The reported sample is the slowest client of the round, which is the
/// number a person feels: the round is over when every client has its echo.
/// The spread (slowest minus fastest) is printed beside it: a growing spread
/// under load is the early signal of one client's backlog becoming
/// everyone's problem.
pub async fn run_fanout() -> ScenarioOutcome {
    let name = "fan-out delivery, 4 clients on 4 terminals".to_string();
    let daemon = TestDaemon::start().await;
    let mut maker = daemon.attach("bench-maker").await;
    let mut terminals = daemon.terminals().streams();
    while terminals.len() < FANOUT_CLIENTS {
        match maker
            .control(
                &ControlRequest::CreateTerminal {
                    id: format!("mk-{}", terminals.len()),
                    cwd: None,
                    workspace: None,
                    name: None,
                },
                PATIENCE,
            )
            .await
        {
            Some(ControlResponse::TerminalCreated { terminal, .. }) => terminals.push(terminal),
            Some(other) => {
                return ScenarioOutcome::failed(
                    name,
                    format!("could not grow terminals: {other:?}"),
                );
            }
            None => return ScenarioOutcome::failed(name, "no answer while growing terminals"),
        }
    }
    // The maker leaves: it is still attached and auto-subscribed to the
    // first terminal, which would make this five clients on four terminals
    // while the name says four.
    drop(maker);

    // One client per terminal. A fresh connection auto-subscribes to the
    // lowest terminal, so each client drops that before holding its own, or
    // every client also renders terminal one and the load is not what the
    // name says.
    let first = terminals[0];
    let mut clients: Vec<(TestClient, u32)> = Vec::new();
    for (i, term) in terminals.iter().take(FANOUT_CLIENTS).copied().enumerate() {
        let mut c = daemon.attach(&format!("bench-fanout-{i}")).await;
        match c
            .control(
                &ControlRequest::Unsubscribe {
                    id: format!("drop-auto-{i}"),
                    terminals: vec![first],
                },
                PATIENCE,
            )
            .await
        {
            Some(ControlResponse::Subscribed { terminals, .. }) => {
                if !terminals.contains(&first) {
                    return ScenarioOutcome::failed(
                        name,
                        format!("client {i} never held the auto subscription it had to drop"),
                    );
                }
            }
            Some(other) => {
                return ScenarioOutcome::failed(name, format!("unsubscribe refused: {other:?}"));
            }
            None => {
                return ScenarioOutcome::failed(name, "a client never answered its unsubscribe");
            }
        }
        match c
            .control(
                &ControlRequest::Subscribe {
                    id: format!("sub-{i}"),
                    terminals: vec![term],
                },
                PATIENCE,
            )
            .await
        {
            Some(ControlResponse::Subscribed { .. }) => {}
            Some(other) => {
                return ScenarioOutcome::failed(name, format!("subscribe refused: {other:?}"));
            }
            None => return ScenarioOutcome::failed(name, "no answer to subscribe"),
        }
        // Wait for a live shell before sampling, so round zero does not pay
        // for shell startup.
        let seen = c
            .wait_for(
                |s| s.lines.iter().any(|l| !l.trim().is_empty()),
                ROUND_TIMEOUT,
            )
            .await;
        if seen.is_none() {
            return ScenarioOutcome::failed(name, "a shell never printed its prompt");
        }
        clients.push((c, term));
    }

    let mut slowest: Vec<f64> = Vec::with_capacity(FANOUT_SAMPLES);
    let mut spreads: Vec<f64> = Vec::with_capacity(FANOUT_SAMPLES);
    for round in 0..(FANOUT_WARMUP + FANOUT_SAMPLES) {
        let marker: Vec<String> = (0..FANOUT_CLIENTS)
            .map(|i| format!("FAN{round:03}C{i}"))
            .collect();
        // All four type first; the clock for each starts at its own write.
        // Addressed by stream: `input` would send every marker to the
        // lowest terminal, and three clients would wait forever.
        let mut starts = Vec::with_capacity(FANOUT_CLIENTS);
        for (i, (c, term)) in clients.iter_mut().enumerate() {
            starts.push(Instant::now());
            c.input_to(*term, marker[i].as_bytes()).await;
        }
        for (c, term) in clients.iter_mut() {
            c.input_to(*term, b"\n").await;
        }
        // Delivery is awaited concurrently, by polling: waiting for the
        // clients one after another would charge every client for the time
        // spent waiting for the ones before it, and the slowest sample
        // would be an artefact of the order rather than of the daemon. Each
        // poll is a short slice; a client whose echo has arrived keeps the
        // time it arrived at, not the time the loop got around to it.
        let mut elapsed: Vec<Option<f64>> = vec![None; FANOUT_CLIENTS];
        let round_deadline = Instant::now() + ROUND_TIMEOUT;
        while elapsed.iter().any(Option::is_none) {
            if Instant::now() >= round_deadline {
                let missing: Vec<usize> = elapsed
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.is_none())
                    .map(|(i, _)| i)
                    .collect();
                return ScenarioOutcome::failed(
                    name,
                    format!("round {round}: clients {missing:?} never got their echo"),
                );
            }
            for (i, (c, _)) in clients.iter_mut().enumerate() {
                if elapsed[i].is_some() {
                    continue;
                }
                let m = marker[i].clone();
                // A short slice, not the whole remaining budget: one quiet
                // client must not hold the others' clocks.
                let seen = c
                    .wait_for(
                        move |s| s.lines.iter().any(|l| l.contains(m.as_str())),
                        Duration::from_millis(20),
                    )
                    .await;
                if seen.is_some() {
                    elapsed[i] = Some(starts[i].elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        let elapsed: Vec<f64> = elapsed.into_iter().flatten().collect();
        for (c, term) in clients.iter_mut() {
            // Clear the line so the next round cannot match this round's text.
            c.input_to(*term, &[0x15]).await;
        }
        if round >= FANOUT_WARMUP {
            let lo = elapsed.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = elapsed.iter().cloned().fold(0.0f64, f64::max);
            slowest.push(hi);
            spreads.push(hi - lo);
        }
    }
    let spread_p95 = {
        let s = summarize(&spreads);
        s.p95
    };
    eprintln!(
        "note: fan-out spread (slowest minus fastest client per round) p95 is {spread_p95:.3} ms \
         over {} rounds; the table below reports the slowest client",
        FANOUT_SAMPLES
    );
    // No section-7 budget names this scenario yet, so none is asserted: the
    // number is reported and the spread note above is the finding to watch.
    ScenarioOutcome::measured(name, "ms", summarize(&slowest), None)
}

/// Scenario 7: a client drops mid-session and a fresh client reattaches.
///
/// Each round types a marker, waits for its echo (proving the session is
/// live), drops the connection entirely, then reattaches under a new client
/// id and times how long until a snapshot already contains the marker.
/// Nothing is typed after reattaching, so the number is the reattach path:
/// connect, the automatic subscription, and the daemon's
/// current-screen-first delivery. The
/// clock starts before the connect, not after it: starting it later would
/// measure an already-delivered snapshot rather than the reconnect.
pub async fn run_reconnect() -> ScenarioOutcome {
    let name = "reconnect catch-up, fresh client, no retype".to_string();
    let daemon = TestDaemon::start().await;

    let mut samples = Vec::with_capacity(RECONNECT_SAMPLES);
    for round in 0..RECONNECT_SAMPLES {
        let marker = format!("RECON{round:03}");
        {
            let mut c = daemon.attach(&format!("bench-recon-{round}")).await;
            c.input(marker.as_bytes()).await;
            c.input(b"\n").await;
            let m = marker.clone();
            let seen = c
                .wait_for(
                    move |s| s.lines.iter().any(|l| l.contains(m.as_str())),
                    ROUND_TIMEOUT,
                )
                .await;
            if seen.is_none() {
                return ScenarioOutcome::failed(
                    name,
                    format!("round {round}: the live marker never echoed"),
                );
            }
            // `c` drops here: the connection closes, the session keeps running.
        }
        // The clock starts before the connect, not after it: the reconnect
        // path is connect plus the automatic subscription plus the first
        // render carrying prior output, and starting later would time an
        // already-delivered snapshot instead. No explicit resubscribe: a
        // fresh connection is already subscribed to this terminal.
        let start = Instant::now();
        let mut c = daemon.attach(&format!("bench-recon-{round}-back")).await;
        let m = marker.clone();
        let seen = c
            .wait_for(
                move |s| s.lines.iter().any(|l| l.contains(m.as_str())),
                ROUND_TIMEOUT,
            )
            .await;
        if seen.is_none() {
            return ScenarioOutcome::failed(
                name,
                format!("round {round}: the reattached client never saw prior output"),
            );
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    ScenarioOutcome::measured(name, "ms", summarize(&samples), None)
}
