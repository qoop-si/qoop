//! `qoop-bench`: the milestone 1 performance harness named in the plan's section 7.
//!
//! Not a criterion-style microbenchmark suite. Every number this prints comes from driving
//! the real daemon: through `qoop-testkit` where an in-process daemon is enough, and through
//! the real `qoro` binary where a separate process is the thing being measured. A scenario
//! that cannot be measured says so and the run exits non-zero for it; it never prints a
//! plausible number in its place. A missed budget, by contrast, is reported and does not by
//! itself fail the run: section 7 states the budgets are targets pending this harness's
//! first run, not gates yet.
//!
//! Scenarios, in the plan's section 7 order:
//! 1. `qoro --version` wall time.
//! 2. attach latency: real client process start to first rendered frame.
//! 3. input latency, local and quiet.
//! 4. input latency while the terminal streams about 1 MiB/s to itself.
//! 5. resident memory per terminal, at two sizes.
//! 6. fan-out delivery: four clients on four terminals, slowest client wins.
//! 7. reconnect catch-up: a fresh client sees prior output with no retype.

mod proc;
mod report;
mod scenarios;
mod stats;
mod wire;

use report::ScenarioOutcome;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    let bin = proc::bin_path();
    println!("qoro binary under test: {}", bin.display());
    let git_rev = git_rev();
    match &git_rev {
        Some(rev) => println!("git rev: {rev}"),
        None => println!("git rev: unavailable (no commit checked out, or not a git checkout)"),
    }
    // Said plainly, because a latency figure means nothing without the path it measures.
    // The input latency scenarios drive a real daemon over a real unix socket, but that
    // daemon runs inside this process, so the numbers exclude one process boundary and the
    // client's own terminal handling. The version and attach scenarios do run the real
    // binary, and say so by being slower.
    println!(
        "input latency path: real socket, real pty, real shell echo; daemon in this process, \
         so one process boundary and the client's terminal handling are not included"
    );

    let mut outcomes: Vec<ScenarioOutcome> = Vec::new();
    outcomes.push(scenarios::version::run(&bin));
    outcomes.push(scenarios::attach::run(&bin));
    outcomes.push(scenarios::input_latency::run_quiet().await);
    outcomes.push(scenarios::input_latency::run_noisy().await);
    outcomes.extend(scenarios::memory::run(&bin).await);
    outcomes.push(scenarios::fanout::run_fanout().await);
    outcomes.push(scenarios::fanout::run_reconnect().await);

    println!();
    report::print_table(&outcomes);
    println!();
    report::print_jsonl(&outcomes, git_rev.as_deref());

    if outcomes.iter().all(ScenarioOutcome::measured_ok) {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// The short commit hash of the working tree, when one exists. `None` rather than a fatal
/// error: a shallow checkout or an unborn HEAD is a normal state for a benchmark run, and
/// the JSONL schema's `git_rev` field is documented as "when available" for exactly this.
fn git_rev() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rev = String::from_utf8(out.stdout).ok()?;
    let rev = rev.trim();
    if rev.is_empty() {
        None
    } else {
        Some(rev.to_string())
    }
}
