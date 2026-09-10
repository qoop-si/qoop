//! The shared result type every scenario returns, and its two renderings: a human table and
//! a JSONL record.
//!
//! Nothing in this file computes a number. Every field is either a real measurement, already
//! summarized by `crate::stats`, or a fixed budget copied from the plan's section 7, kept
//! here only so the report can show the two side by side.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::stats::Percentiles;

/// Which summary statistic a budget is stated against. Section 7 states most latency
/// budgets at p95 and the quiet-input budget at p50; the memory budgets are per-terminal
/// figures compared here against p50 of a small repeat count, documented at the call site.
#[derive(Debug, Clone, Copy)]
pub enum CheckStat {
    P50,
    P95,
}

impl CheckStat {
    fn label(self) -> &'static str {
        match self {
            Self::P50 => "p50",
            Self::P95 => "p95",
        }
    }

    fn value(self, stats: &Percentiles) -> f64 {
        match self {
            Self::P50 => stats.p50,
            Self::P95 => stats.p95,
        }
    }
}

pub struct Budget {
    pub stat: CheckStat,
    pub limit: f64,
    pub description: &'static str,
}

pub enum ScenarioOutcome {
    Measured {
        name: String,
        unit: &'static str,
        stats: Percentiles,
        budget: Option<Budget>,
    },
    Failed {
        name: String,
        reason: String,
    },
}

impl ScenarioOutcome {
    pub fn failed(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Failed {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn measured(
        name: impl Into<String>,
        unit: &'static str,
        stats: Percentiles,
        budget: Option<Budget>,
    ) -> Self {
        Self::Measured {
            name: name.into(),
            unit,
            stats,
            budget,
        }
    }

    /// Whether this scenario produced a real number at all. A missed budget is a finding to
    /// report, not a measurement failure, so it does not affect this; see the plan's
    /// section 7.
    pub fn measured_ok(&self) -> bool {
        matches!(self, Self::Measured { .. })
    }

    /// The budget comparison line and whether it passed, if this scenario has a stated
    /// budget.
    fn budget_check(&self) -> Option<(String, bool)> {
        match self {
            Self::Failed { .. } => None,
            Self::Measured { stats, budget, .. } => {
                let b = budget.as_ref()?;
                let measured = b.stat.value(stats);
                let pass = measured <= b.limit;
                Some((
                    format!(
                        "{} {:.3} vs budget {:.3} ({})",
                        b.stat.label(),
                        measured,
                        b.limit,
                        b.description
                    ),
                    pass,
                ))
            }
        }
    }
}

pub fn print_table(outcomes: &[ScenarioOutcome]) {
    println!(
        "{:<48} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10} {:>5}  budget",
        "scenario", "unit", "p50", "p90", "p95", "p99", "max", "n"
    );
    for o in outcomes {
        match o {
            ScenarioOutcome::Measured {
                name, unit, stats, ..
            } => {
                let (mark, budget_text) = match o.budget_check() {
                    Some((text, true)) => ("PASS", text),
                    Some((text, false)) => ("FAIL", text),
                    None => ("----", "no stated budget".to_string()),
                };
                println!(
                    "{name:<48} {unit:>6} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>5}  {mark} {budget_text}",
                    stats.p50, stats.p90, stats.p95, stats.p99, stats.max, stats.samples
                );
            }
            ScenarioOutcome::Failed { name, reason } => {
                println!(
                    "{name:<48} {:>6} {:>10} {:>10} {:>10} {:>10} {:>10} {:>5}  FAIL could not measure: {reason}",
                    "-", "-", "-", "-", "-", "-", "-"
                );
            }
        }
    }
}

#[derive(Serialize)]
struct JsonlRow<'a> {
    name: &'a str,
    unit: Option<&'a str>,
    p50: Option<f64>,
    p90: Option<f64>,
    p99: Option<f64>,
    samples: usize,
    timestamp_unix: u64,
    git_rev: Option<&'a str>,
    error: Option<&'a str>,
}

/// One JSONL row per scenario: name, unit, p50, p90, p99, samples, timestamp, git rev when
/// available. A scenario that could not be measured still gets a row, with `error` set and
/// every numeric field absent rather than a plausible-looking zero.
pub fn print_jsonl(outcomes: &[ScenarioOutcome], git_rev: Option<&str>) {
    let timestamp_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for o in outcomes {
        let row = match o {
            ScenarioOutcome::Measured {
                name, unit, stats, ..
            } => JsonlRow {
                name,
                unit: Some(unit),
                p50: Some(stats.p50),
                p90: Some(stats.p90),
                p99: Some(stats.p99),
                samples: stats.samples,
                timestamp_unix,
                git_rev,
                error: None,
            },
            ScenarioOutcome::Failed { name, reason } => JsonlRow {
                name,
                unit: None,
                p50: None,
                p90: None,
                p99: None,
                samples: 0,
                timestamp_unix,
                git_rev,
                error: Some(reason),
            },
        };
        println!(
            "{}",
            serde_json::to_string(&row)
                .expect("a plain struct of strings and numbers always serializes")
        );
    }
}
