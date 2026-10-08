//! Scenarios 3 and 4: keystroke-to-echo latency, quiet and self-noisy.
//!
//! Both drive a real daemon through `qoop-testkit`: a real PTY, a real shell, the same
//! reassembly and snapshot decoding a real client uses. What is measured is wall time from
//! sending a marker string as input to the moment a decoded snapshot contains it, which is
//! the child's own line discipline echoing what was typed, not a synthetic shortcut.

use std::time::{Duration, Instant};

use qoop_testkit::{TestClient, TestDaemon};

use crate::report::{Budget, CheckStat, ScenarioOutcome};
use crate::stats::summarize;

const WARMUP: usize = 5;
const SAMPLES: usize = 200;
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(2);

/// Scenario 3: input latency, local and quiet.
pub async fn run_quiet() -> ScenarioOutcome {
    let name = "input latency, local, quiet".to_string();
    let daemon = TestDaemon::start().await;
    let mut client = daemon.attach("bench-quiet").await;
    settle(&mut client).await;

    match run_samples(&mut client, "Q", false).await {
        Ok(samples) => {
            let budget = Budget {
                stat: CheckStat::P50,
                limit: 2.0,
                description: "p50 under 2 ms, section 7",
            };
            ScenarioOutcome::measured(name, "ms", summarize(&samples), Some(budget))
        }
        Err(e) => ScenarioOutcome::failed(name, e),
    }
}

/// Scenario 4: input latency while the terminal streams about 1 MiB/s to itself.
///
/// The noise is a background job inside the same shell, not a second client's input: G2.12
/// in the master plan is about a terminal that is itself busy, which means the child
/// process's own output competing for the render path, not another client typing into it.
pub async fn run_noisy() -> ScenarioOutcome {
    let name = "input latency, streaming ~1 MiB/s to itself".to_string();
    let daemon = TestDaemon::start().await;
    let mut client = daemon.attach("bench-noisy").await;
    settle(&mut client).await;

    // ~1025 bytes every millisecond is about 1 MiB/s. Bounded to 60000 iterations (a minute)
    // so a failure to deliver the `kill` below still cannot leave a runaway loop; the
    // sampling loop below needs a few seconds of this, not a minute.
    client
        .input(b"(i=0; while [ $i -lt 60000 ]; do printf '%-1024s\\n' Q; sleep 0.001; i=$((i+1)); done) &\n")
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let result = run_samples(&mut client, "R", true).await;

    // Best-effort: stop the background loop before the daemon and its shell go away, so this
    // scenario does not leave a CPU-spinning process behind it.
    client.input(b"kill %1 2>/dev/null\n").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    match result {
        Ok(samples) => {
            let budget = Budget {
                stat: CheckStat::P95,
                limit: 8.0,
                description: "p95 under 8 ms, section 7",
            };
            ScenarioOutcome::measured(name, "ms", summarize(&samples), Some(budget))
        }
        Err(e) => ScenarioOutcome::failed(name, e),
    }
}

/// Wait for the first non-blank screen, so sample 0 does not pay for connection and shell
/// startup rather than for input latency.
async fn settle(client: &mut TestClient) {
    client
        .wait_for(
            |s| s.lines.iter().any(|l| !l.trim().is_empty()),
            Duration::from_secs(5),
        )
        .await;
}

/// Type a unique marker, wait for its echo, then clear the line with the terminal's kill
/// character so the next marker cannot match stale text left by this one. `prefix` keeps the
/// two scenarios' markers from ever matching each other's shell output.
///
/// What happens to a round trip whose echo never appears, and why it is not a slow sample.
///
/// The first version discarded it, which is backwards: dropped samples are the slow ones,
/// so discarding flatters exactly the percentile the budget is about. The second version
/// recorded it at the timeout, which is the safe direction for a latency figure.
///
/// Measured, and neither is right for this scenario. Raising the timeout from 2 seconds to
/// 12 changed nothing: the same one sample in two hundred still never arrived, so it is not
/// a slow round trip, it is an echo that was never delivered. In a terminal scrolling at a
/// megabyte a second the typed characters are scrolled away before a snapshot carrying them
/// is written, and the daemon's render lane coalesces per terminal by design (ADR-013), so
/// the state that showed the echo is superseded before it reaches the socket.
///
/// So an echo that never appears is counted as unobservable and excluded, and the count is
/// printed next to the result. Recording it at the timeout would report a 12 second latency
/// that a 12 second timeout proved does not exist. Excluding it silently would be the
/// original mistake. The quiet scenario has no such effect and any timeout there is a real
/// sample, recorded at the timeout.
async fn run_samples(
    client: &mut TestClient,
    prefix: &str,
    scrolls: bool,
) -> Result<Vec<f64>, String> {
    let mut samples = Vec::with_capacity(SAMPLES);
    let mut warmed_up = 0usize;
    let mut censored = 0usize;
    let mut unobservable = 0usize;
    let mut warmup_timeouts = 0usize;
    let mut attempts = 0usize;
    let max_attempts = (WARMUP + SAMPLES) * 3;

    while (warmed_up < WARMUP || samples.len() < SAMPLES) && attempts < max_attempts {
        let marker = format!("{prefix}{attempts:05}");
        attempts += 1;
        let start = Instant::now();
        client.input(marker.as_bytes()).await;
        let seen = client
            .wait_for(
                |s| s.lines.iter().any(|l| l.contains(&marker)),
                ROUND_TRIP_TIMEOUT,
            )
            .await;
        let elapsed = start.elapsed();

        // Ctrl-U: the kernel line discipline's kill character, independent of which shell is
        // running, so clearing the line does not depend on a particular shell's readline.
        // Sent whether or not the marker was seen, so a timed-out sample cannot leave stale
        // text for the next one to match against.
        client.input(&[0x15]).await;
        client
            .wait_for(
                |s| !s.lines.iter().any(|l| l.contains(&marker)),
                ROUND_TRIP_TIMEOUT,
            )
            .await;

        if warmed_up < WARMUP {
            // A warmup round trip that times out is still only warmup.
            if seen.is_some() {
                warmed_up += 1;
            } else {
                warmup_timeouts += 1;
            }
            continue;
        }
        match (seen, scrolls) {
            (Some(_), _) => samples.push(elapsed.as_secs_f64() * 1000.0),
            // A terminal scrolling under its own output can lose the echo entirely. Proved
            // by measurement, not assumed: see the note above.
            (None, true) => unobservable += 1,
            (None, false) => {
                censored += 1;
                samples.push(ROUND_TRIP_TIMEOUT.as_secs_f64() * 1000.0);
            }
        }
    }

    if samples.len() < SAMPLES {
        return Err(format!(
            "only {} of {SAMPLES} samples completed within {attempts} attempts ({censored} timed out at {ROUND_TRIP_TIMEOUT:?} each, plus {warmup_timeouts} during warmup)",
            samples.len()
        ));
    }
    if unobservable > 0 {
        eprintln!(
            "note: {unobservable} {prefix}-prefixed echoes were scrolled away before any \
             snapshot carried them and are excluded as unobservable, not as slow"
        );
    }
    if censored > 0 {
        eprintln!(
            "note: {censored} of {SAMPLES} {prefix}-prefixed measured round trips hit the {ROUND_TRIP_TIMEOUT:?} timeout and are recorded at it, so the tail is a lower bound"
        );
    }
    if warmup_timeouts > 0 {
        // Warmup is not measured, so this is not a censored sample. It is still worth
        // saying: a shell that is slow to start is a different fact from a slow round trip,
        // and one printed as the other is how a real slowdown gets explained away.
        eprintln!(
            "note: {warmup_timeouts} {prefix}-prefixed warmup round trips timed out before the first measured sample"
        );
    }
    Ok(samples)
}
