//! Scenario 1: `qoop --version` wall time.
//!
//! Runs the real binary because there is nothing else this scenario could measure: process
//! spawn overhead, argument parsing and the write to stdout are exactly what a user pays for
//! `qoop --version`, and a criterion-style in-process call would measure a different thing
//! wearing this scenario's name.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::report::{Budget, CheckStat, ScenarioOutcome};
use crate::stats::summarize;

const WARMUP: usize = 5;
const SAMPLES: usize = 50;

pub fn run(bin: &Path) -> ScenarioOutcome {
    let name = "qoop --version".to_string();
    if !bin.is_file() {
        return ScenarioOutcome::failed(
            name,
            format!(
                "binary not found at {}; build it first with `cargo build -p qoop-cli`",
                bin.display()
            ),
        );
    }

    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..(WARMUP + SAMPLES) {
        let start = Instant::now();
        let status = match Command::new(bin)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                return ScenarioOutcome::failed(
                    name,
                    format!("failed to spawn {}: {e}", bin.display()),
                );
            }
        };
        let elapsed = start.elapsed();
        if !status.success() {
            return ScenarioOutcome::failed(name, format!("run {i} exited with {status}"));
        }
        if i >= WARMUP {
            samples.push(elapsed.as_secs_f64() * 1000.0);
        }
    }

    let budget = Budget {
        stat: CheckStat::P95,
        limit: 50.0,
        description: "p95 under 50 ms, section 7",
    };
    ScenarioOutcome::measured(name, "ms", summarize(&samples), Some(budget))
}
