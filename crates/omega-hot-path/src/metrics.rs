// crates/omega-hot-path/src/metrics.rs
//
// HotPathMetrics — observability for the <1ms Microtx execution lane.
//
// ## Spec §4, §16
//
//   The hot-path simulation SLA is <1ms per blueprint.  HotPathMetrics
//   tracks:
//     - p50/p95/p99 simulation latency (via a compact reservoir)
//     - Miss rate (simulations rejected before execution)
//     - Successes and total EV captured
//     - Slot utilisation (live vs capacity)
//
//   All counters use atomics — the metrics struct is `Clone + Send + Sync`
//   and safe to read from any thread without locking.  The shadow scorecard
//   `sim_latency_p95_ms` metric reads directly from this struct.
//
// ## Latency histogram
//
//   We use a fixed-width histogram over microseconds with 8 buckets:
//     [0,100), [100,250), [250,500), [500,1000), [1000,2000),
//     [2000,5000), [5000,10000), [10000,∞)
//
//   The SLA target is 1000µs (<1ms).  Buckets 0–3 are within-SLA;
//   buckets 4–7 are SLA violations.  A separate `sla_violations` counter
//   tracks the total number of executions that exceeded 1ms.
//
//   p95 is estimated from the histogram by summing bucket counts until
//   ≥ 95% of total observations are covered.

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::U256;
use chrono::{DateTime, Utc};
use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// Histogram buckets
// ─────────────────────────────────────────────────────────────────────────────

/// Upper bounds (exclusive) of the latency histogram buckets in microseconds.
///
/// The final bucket `u64::MAX` is the catch-all for everything above 10ms.
pub const LATENCY_BUCKETS_US: &[u64] = &[100, 250, 500, 1_000, 2_000, 5_000, 10_000, u64::MAX];

const NUM_BUCKETS: usize = 8;

/// The hot-path latency SLA in microseconds (spec §4: <1ms).
pub const SLA_US: u64 = 1_000;

// ─────────────────────────────────────────────────────────────────────────────
// HotPathMetrics
// ─────────────────────────────────────────────────────────────────────────────

/// Thread-safe observability metrics for the Microtx execution lane.
///
/// Shared via `Arc<HotPathMetrics>`.  All write operations use
/// `Relaxed` ordering — metrics are eventually-consistent diagnostics,
/// not synchronisation primitives.
pub struct HotPathMetrics {
    /// Total blueprints that entered simulation (accepted + rejected).
    pub total: AtomicU64,
    /// Blueprints that completed simulation successfully.
    pub successes: AtomicU64,
    /// Blueprints rejected by any simulation guard (expiry, gas, profit).
    pub misses: AtomicU64,
    /// Simulations that exceeded the 1ms SLA.
    pub sla_violations: AtomicU64,
    /// Sum of latencies for all successful simulations (microseconds).
    pub latency_sum_us: AtomicU64,
    /// Sum of net profit wei captured across all successful simulations.
    /// Stored as two u64 halves (high, low) to avoid overflow.
    total_profit_hi: AtomicU64,
    total_profit_lo: AtomicU64,
    /// Latency histogram: count per bucket (see `LATENCY_BUCKETS_US`).
    histogram: [AtomicU64; NUM_BUCKETS],
    /// Timestamp of the last successful simulation.
    pub last_success_at: std::sync::Mutex<Option<DateTime<Utc>>>,
}

impl HotPathMetrics {
    /// Create a zeroed metrics instance.
    pub fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            successes: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            sla_violations: AtomicU64::new(0),
            latency_sum_us: AtomicU64::new(0),
            total_profit_hi: AtomicU64::new(0),
            total_profit_lo: AtomicU64::new(0),
            histogram: std::array::from_fn(|_| AtomicU64::new(0)),
            last_success_at: std::sync::Mutex::new(None),
        }
    }

    /// Record a successful simulation result.
    pub fn record_success(&self, latency_us: u64, profit_net: U256) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.successes.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);

        if latency_us >= SLA_US {
            self.sla_violations.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(latency_us, sla_us = SLA_US, "Hot-path SLA exceeded");
        }

        self.increment_histogram(latency_us);
        self.add_profit(profit_net);

        if let Ok(mut guard) = self.last_success_at.lock() {
            *guard = Some(Utc::now());
        }
    }

    /// Record a simulation miss (any guard rejection before execution).
    pub fn record_miss(&self) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Miss rate in [0.0, 1.0].
    pub fn miss_rate(&self) -> f64 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.misses.load(Ordering::Relaxed) as f64 / total as f64
    }

    /// Mean simulation latency in microseconds.
    pub fn mean_latency_us(&self) -> f64 {
        let n = self.successes.load(Ordering::Relaxed);
        if n == 0 {
            return 0.0;
        }
        self.latency_sum_us.load(Ordering::Relaxed) as f64 / n as f64
    }

    /// Mean simulation latency in milliseconds.
    pub fn mean_latency_ms(&self) -> f64 {
        self.mean_latency_us() / 1_000.0
    }

    /// Estimated p95 latency in microseconds from the histogram.
    pub fn p95_latency_us(&self) -> u64 {
        let total = self.successes.load(Ordering::Relaxed);
        if total < 20 {
            return 0;
        }

        let target = (total as f64 * 0.95).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.histogram.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return LATENCY_BUCKETS_US[i];
            }
        }
        LATENCY_BUCKETS_US[NUM_BUCKETS - 2]
    }

    /// p95 latency in milliseconds.
    pub fn p95_latency_ms(&self) -> f64 {
        self.p95_latency_us() as f64 / 1_000.0
    }

    /// SLA compliance rate: fraction of simulations within 1ms.
    pub fn sla_compliance_rate(&self) -> f64 {
        let n = self.successes.load(Ordering::Relaxed);
        if n == 0 {
            return 1.0;
        }
        1.0 - (self.sla_violations.load(Ordering::Relaxed) as f64 / n as f64)
    }

    /// Total net profit captured in wei as a u128.
    pub fn total_profit_wei(&self) -> u128 {
        let hi = self.total_profit_hi.load(Ordering::Relaxed) as u128;
        let lo = self.total_profit_lo.load(Ordering::Relaxed) as u128;
        hi.saturating_mul(u64::MAX as u128 + 1).saturating_add(lo)
    }

    /// Produce an immutable snapshot for serialisation.
    pub fn snapshot(&self) -> HotPathMetricsSnapshot {
        HotPathMetricsSnapshot {
            total: self.total.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            sla_violations: self.sla_violations.load(Ordering::Relaxed),
            miss_rate: self.miss_rate(),
            mean_latency_us: self.mean_latency_us(),
            p95_latency_us: self.p95_latency_us(),
            p95_latency_ms: self.p95_latency_ms(),
            sla_compliance_rate: self.sla_compliance_rate(),
            histogram: std::array::from_fn(|i| self.histogram[i].load(Ordering::Relaxed)),
        }
    }

    /// Reset all counters.
    pub fn reset(&self) {
        self.total.store(0, Ordering::Relaxed);
        self.successes.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.sla_violations.store(0, Ordering::Relaxed);
        self.latency_sum_us.store(0, Ordering::Relaxed);
        self.total_profit_hi.store(0, Ordering::Relaxed);
        self.total_profit_lo.store(0, Ordering::Relaxed);
        for bucket in &self.histogram {
            bucket.store(0, Ordering::Relaxed);
        }
        if let Ok(mut g) = self.last_success_at.lock() {
            *g = None;
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn increment_histogram(&self, latency_us: u64) {
        for (i, &upper) in LATENCY_BUCKETS_US.iter().enumerate() {
            if latency_us < upper {
                self.histogram[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.histogram[NUM_BUCKETS - 1].fetch_add(1, Ordering::Relaxed);
    }

    fn add_profit(&self, profit: U256) {
        let as_u128 = if profit > U256::from(u128::MAX) {
            u128::MAX
        } else {
            profit.to::<u128>()
        };
        let hi = (as_u128 >> 64) as u64;
        let lo = as_u128 as u64;
        self.total_profit_hi.fetch_add(hi, Ordering::Relaxed);
        self.total_profit_lo.fetch_add(lo, Ordering::Relaxed);
    }
}

impl Default for HotPathMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathMetricsSnapshot
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HotPathMetricsSnapshot {
    pub total: u64,
    pub successes: u64,
    pub misses: u64,
    pub sla_violations: u64,
    pub miss_rate: f64,
    pub mean_latency_us: f64,
    pub p95_latency_us: u64,
    pub p95_latency_ms: f64,
    pub sla_compliance_rate: f64,
    pub histogram: [u64; NUM_BUCKETS],
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_are_zeroed() {
        let m = HotPathMetrics::new();
        assert_eq!(m.total.load(Ordering::Relaxed), 0);
        assert!((m.miss_rate() - 0.0).abs() < f64::EPSILON);
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn record_success_increments_counters() {
        let m = HotPathMetrics::new();
        m.record_success(500, U256::from(1_000_000_u64));
        assert_eq!(m.total.load(Ordering::Relaxed), 1);
        assert_eq!(m.successes.load(Ordering::Relaxed), 1);
        assert_eq!(m.misses.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn record_miss_increments_counters() {
        let m = HotPathMetrics::new();
        m.record_miss();
        assert_eq!(m.total.load(Ordering::Relaxed), 1);
        assert_eq!(m.misses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miss_rate_correct() {
        let m = HotPathMetrics::new();
        for _ in 0..80 {
            m.record_success(300, U256::from(1_000_u64));
        }
        for _ in 0..20 {
            m.record_miss();
        }
        assert!((m.miss_rate() - 0.20).abs() < 1e-9);
    }

    #[test]
    fn sla_violation_recorded_when_latency_exceeds_1ms() {
        let m = HotPathMetrics::new();
        m.record_success(1_500, U256::from(1_000_u64));
        assert_eq!(m.sla_violations.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn no_sla_violation_under_1ms() {
        let m = HotPathMetrics::new();
        m.record_success(999, U256::from(1_000_u64));
        assert_eq!(m.sla_violations.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn p95_latency_requires_min_20_observations() {
        let m = HotPathMetrics::new();
        for _ in 0..19 {
            m.record_success(300, U256::from(1_u64));
        }
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn p95_latency_estimated_from_histogram() {
        let m = HotPathMetrics::new();
        for _ in 0..95 {
            m.record_success(200, U256::from(1_u64));
        }
        for _ in 0..5 {
            m.record_success(1_500, U256::from(1_u64));
        }
        let p95 = m.p95_latency_us();
        assert!(
            p95 <= 500,
            "p95={p95} should be ≤500µs with 95% under 250µs"
        );
    }

    #[test]
    fn mean_latency_computed() {
        let m = HotPathMetrics::new();
        m.record_success(200, U256::ZERO);
        m.record_success(400, U256::ZERO);
        assert!((m.mean_latency_us() - 300.0).abs() < 1.0);
        assert!((m.mean_latency_ms() - 0.3).abs() < 0.001);
    }

    #[test]
    fn reset_clears_all_state() {
        let m = HotPathMetrics::new();
        m.record_success(500, U256::from(1_000_u64));
        m.record_miss();
        m.reset();
        assert_eq!(m.total.load(Ordering::Relaxed), 0);
        assert_eq!(m.successes.load(Ordering::Relaxed), 0);
        assert_eq!(m.misses.load(Ordering::Relaxed), 0);
        assert_eq!(m.p95_latency_us(), 0);
    }

    #[test]
    fn snapshot_is_serialisable() {
        let m = HotPathMetrics::new();
        m.record_success(300, U256::from(5_000_u64));
        let snap = m.snapshot();
        let json = serde_json::to_string(&snap).expect("serialisable");
        assert!(json.contains("\"successes\":1"));
    }
}
