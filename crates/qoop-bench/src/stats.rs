//! Percentile computation, nearest rank method.
//!
//! Section 7 of the master plan: "Percentiles by nearest rank, stated so two runs are
//! comparable." Nearest rank is deterministic and needs no interpolation choice to document,
//! which is the property that makes two independently run reports comparable at all.

/// Nearest-rank percentile of a sorted slice. `p` is in `0.0..=100.0`. `sorted` must be
/// non-empty and already sorted ascending.
fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// p50, p90, p95 and p99 of a sample set, plus the sample count they were computed from.
///
/// p95 is included alongside the p50/p90/p99 the JSONL schema asks for because most of
/// section 7's budgets are stated at p95, and a report cannot show "budget met" against a
/// number it never computed.
#[derive(Debug, Clone, Copy)]
pub struct Percentiles {
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub samples: usize,
}

/// Summarize a sample set. Panics on an empty slice: an empty sample set is a caller bug, not
/// a scenario outcome, and every call site here only reaches this after collecting at least
/// one real measurement.
pub fn summarize(samples: &[f64]) -> Percentiles {
    assert!(!samples.is_empty(), "summarize called with no samples");
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| {
        a.partial_cmp(b)
            .expect("a latency or memory sample is never NaN")
    });
    Percentiles {
        p50: nearest_rank(&sorted, 50.0),
        p90: nearest_rank(&sorted, 90.0),
        p95: nearest_rank(&sorted, 95.0),
        p99: nearest_rank(&sorted, 99.0),
        max: *sorted.last().expect("checked non-empty above"),
        samples: sorted.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_matches_a_hand_worked_example() {
        // 10 samples, 1..=10. p50 -> rank 5 -> value 5. p90 -> rank 9 -> value 9.
        let samples: Vec<f64> = (1..=10).map(|n| n as f64).collect();
        let s = summarize(&samples);
        assert_eq!(s.p50, 5.0);
        assert_eq!(s.p90, 9.0);
        assert_eq!(s.p99, 10.0);
        assert_eq!(s.max, 10.0);
        assert_eq!(s.samples, 10);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let s = summarize(&[42.0]);
        assert_eq!(
            (s.p50, s.p90, s.p95, s.p99, s.max),
            (42.0, 42.0, 42.0, 42.0, 42.0)
        );
    }
}
