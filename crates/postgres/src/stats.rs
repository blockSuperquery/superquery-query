//! Query statistics, for the metrics endpoint (Milestone 14).
//!
//! Lock-free counters on the `Database` handle. Recording sits on every
//! statement's path, so it is a few relaxed atomic adds and nothing that can
//! block or allocate.
//!
//! Database latency is reported separately from request latency on purpose:
//! a slow request with fast statements points at resolvers (an N+1 storm, say),
//! while slow statements point at the database or a missing index.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bucket upper bounds, in seconds. Spans a sub-millisecond index seek to a
/// statement long enough to be near the request timeout.
pub const LATENCY_BUCKETS: [f64; 12] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];

/// A cumulative latency histogram in the Prometheus shape.
#[derive(Debug, Default)]
pub struct Histogram {
    /// `buckets[i]` counts observations `<= LATENCY_BUCKETS[i]`.
    buckets: [AtomicU64; LATENCY_BUCKETS.len()],
    count: AtomicU64,
    sum_micros: AtomicU64,
}

/// A point-in-time copy of a [`Histogram`].
#[derive(Debug, Clone, PartialEq)]
pub struct HistogramSnapshot {
    /// `(upper bound in seconds, cumulative count)`.
    pub buckets: Vec<(f64, u64)>,
    pub count: u64,
    pub sum_seconds: f64,
}

impl Histogram {
    pub fn observe(&self, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        for (bucket, bound) in self.buckets.iter().zip(LATENCY_BUCKETS) {
            if secs <= bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_micros.fetch_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            buckets: self
                .buckets
                .iter()
                .zip(LATENCY_BUCKETS)
                .map(|(b, bound)| (bound, b.load(Ordering::Relaxed)))
                .collect(),
            count: self.count.load(Ordering::Relaxed),
            sum_seconds: self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        }
    }
}

/// Counters for every statement run through [`crate::Database`].
#[derive(Debug, Default)]
pub struct DbStats {
    queries: AtomicU64,
    errors: AtomicU64,
    /// Pool acquire plus execution — what the resolver actually waited for.
    /// Pool waits are included deliberately: under saturation they *are* the
    /// latency.
    pub latency: Histogram,
}

impl DbStats {
    pub fn record(&self, elapsed: Duration, ok: bool) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        if !ok {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        self.latency.observe(elapsed);
    }

    pub fn queries(&self) -> u64 {
        self.queries.load(Ordering::Relaxed)
    }

    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_cumulative() {
        let h = Histogram::default();
        h.observe(Duration::from_micros(300)); // ≤ 0.0005
        h.observe(Duration::from_millis(20)); // ≤ 0.025
        let s = h.snapshot();

        let at = |bound: f64| s.buckets.iter().find(|(b, _)| *b == bound).unwrap().1;
        assert_eq!(at(0.0005), 1);
        assert_eq!(at(0.01), 1);
        assert_eq!(at(0.025), 2);
        assert_eq!(at(2.5), 2);
        assert_eq!(s.count, 2);
    }

    #[test]
    fn slower_than_every_bucket_still_counts() {
        // Only the implicit +Inf bucket (= count) holds it.
        let h = Histogram::default();
        h.observe(Duration::from_secs(10));
        let s = h.snapshot();
        assert!(s.buckets.iter().all(|(_, n)| *n == 0));
        assert_eq!(s.count, 1);
        assert!((s.sum_seconds - 10.0).abs() < 1e-9);
    }

    #[test]
    fn errors_are_counted_alongside_queries() {
        let s = DbStats::default();
        s.record(Duration::from_millis(1), true);
        s.record(Duration::from_millis(1), false);
        assert_eq!(s.queries(), 2);
        assert_eq!(s.errors(), 1);
    }
}
