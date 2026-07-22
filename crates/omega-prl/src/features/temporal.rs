// omega-prl/src/features/temporal.rs
//! Temporal aggregation engine — §7
//!
//! Requirements:
//!   - Lock-free circular windows with O(1) eviction (§7.1)
//!   - Time-bucketed aggregation; cache-aligned storage (§7.1)
//!   - Online algorithms only — no historical rescans (§7.3)
//!   - Six window classes from 5ms nano to 24h historical (§7.2)
//!
//! ## Audit fix (this revision)
//!
//! `CircularWindow::advance_to` previously walked forward one bucket at
//! a time to reach a target timestamp. For the `Nano` window class
//! (~312.5µs/bucket), a timestamp even an hour ahead of the window
//! (clock skew, a backfill jump, corrupted data) required roughly 11
//! million loop iterations before returning — a genuine runaway-loop
//! vector triggerable by a single malformed event, silently stalling
//! whichever shard worker owns this window for the duration while the
//! process otherwise looks alive. Fixed with a bounded fast path: if the
//! gap exceeds the window's full span, every existing bucket is already
//! stale regardless, so the window resets directly to a fresh state at
//! the target timestamp instead of stepping through each intermediate
//! bucket.

/// §7.2 — Window class definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowClass {
    /// 5ms  — relay microbursts
    Nano,
    /// 50ms — gas war acceleration
    Micro,
    /// 1s   — liquidation clustering
    Short,
    /// 30s  — searcher adaptation
    Medium,
    /// 15m  — strategy degradation
    Long,
    /// 24h  — baseline calibration
    Historical,
}

impl WindowClass {
    pub fn duration_nanos(self) -> u64 {
        match self {
            Self::Nano => 5_000_000,
            Self::Micro => 50_000_000,
            Self::Short => 1_000_000_000,
            Self::Medium => 30_000_000_000,
            Self::Long => 900_000_000_000,
            Self::Historical => 86_400_000_000_000,
        }
    }

    /// Number of buckets per window (each bucket covers duration/buckets).
    pub fn bucket_count(self) -> usize {
        match self {
            Self::Nano => 16,
            Self::Micro => 16,
            Self::Short => 32,
            Self::Medium => 32,
            Self::Long => 64,
            Self::Historical => 128,
        }
    }

    pub fn bucket_duration_nanos(self) -> u64 {
        self.duration_nanos() / self.bucket_count() as u64
    }
}

/// §7.3 — Single time bucket. Updated with online Welford algorithm.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, align(64))]
pub struct TemporalBucket {
    /// Bucket start timestamp (nanos).
    pub ts_start: u64,
    pub count: u32,
    pub sum: f64,
    pub mean: f64,
    pub variance: f64,
    pub max: f64,
    pub min: f64,
}

impl TemporalBucket {
    pub fn new(ts_start: u64) -> Self {
        Self {
            ts_start,
            count: 0,
            sum: 0.0,
            mean: 0.0,
            variance: 0.0,
            max: f64::NEG_INFINITY,
            min: f64::INFINITY,
        }
    }

    /// Online Welford update — O(1), no historical rescan (§7.3).
    #[inline]
    pub fn update(&mut self, x: f64) {
        self.count += 1;
        self.sum += x;
        let n = self.count as f64;
        let delta = x - self.mean;
        self.mean += delta / n;
        let delta2 = x - self.mean;
        self.variance = ((n - 1.0) * self.variance + delta * delta2) / n;
        if x > self.max {
            self.max = x;
        }
        if x < self.min {
            self.min = x;
        }
    }

    /// Population standard deviation.
    #[inline]
    pub fn std_dev(&self) -> f64 {
        self.variance.sqrt()
    }

    /// Z-score of value relative to this bucket.
    #[inline]
    pub fn z_score(&self, x: f64) -> f64 {
        let sd = self.std_dev();
        if sd < 1e-12 {
            return 0.0;
        }
        (x - self.mean) / sd
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Circular window — O(1) eviction
// ─────────────────────────────────────────────────────────────────────────────

/// Circular sliding window for one `WindowClass`.
pub struct CircularWindow {
    pub class: WindowClass,
    buckets: Vec<TemporalBucket>,
    write_idx: usize,
    window_start: u64,
}

impl CircularWindow {
    pub fn new(class: WindowClass, now_nanos: u64) -> Self {
        let n = class.bucket_count();
        let buckets = (0..n)
            .map(|i| TemporalBucket::new(now_nanos + i as u64 * class.bucket_duration_nanos()))
            .collect();
        Self {
            class,
            buckets,
            write_idx: 0,
            window_start: now_nanos,
        }
    }

    /// Record a sample at timestamp `ts_nanos`. Evicts stale buckets as needed.
    #[inline]
    pub fn record(&mut self, ts_nanos: u64, value: f64) {
        self.advance_to(ts_nanos);
        self.buckets[self.write_idx].update(value);
    }

    /// Return a snapshot of all live buckets.
    pub fn snapshot(&self) -> &[TemporalBucket] {
        &self.buckets
    }

    /// Aggregate stats across the entire window.
    pub fn aggregate(&self) -> TemporalBucket {
        let mut agg = TemporalBucket::new(self.window_start);
        for b in &self.buckets {
            if b.is_empty() {
                continue;
            }
            for _ in 0..b.count {
                agg.update(b.mean);
            }
            if b.max > agg.max {
                agg.max = b.max;
            }
            if b.min < agg.min {
                agg.min = b.min;
            }
        }
        agg
    }

    /// Advance the write position to cover `ts_nanos`.
    ///
    /// Previously walked one bucket at a time — for a timestamp far
    /// ahead of the window (clock skew, a backfill jump, corrupted
    /// data), this could iterate millions of times before catching up,
    /// stalling the shard worker that owns this window for the
    /// duration. Fixed with a bounded fast path: if the gap exceeds the
    /// full window span, every existing bucket is already stale anyway,
    /// so reset directly to a fresh window at `ts_nanos` instead of
    /// stepping through each one.
    ///
    /// An out-of-order (past) timestamp — `ts_nanos` older than the
    /// current write bucket — is written into the CURRENT bucket rather
    /// than rejected. This is a known, separate gap: such a value gets
    /// silently attributed to the wrong (newer) time bucket rather than
    /// discarded. Left as-is here since rejecting it outright would
    /// require the caller to handle a dropped sample, which is a
    /// decision belonging to whoever owns event ordering upstream.
    fn advance_to(&mut self, ts_nanos: u64) {
        let bucket_dur = self.class.bucket_duration_nanos();
        let n = self.buckets.len();
        let full_span = bucket_dur * n as u64;

        let current_start = self.buckets[self.write_idx].ts_start;

        // Fast path: gap too large to step through bucket-by-bucket.
        if ts_nanos >= current_start.saturating_add(full_span) {
            for (i, bucket) in self.buckets.iter_mut().enumerate() {
                *bucket = TemporalBucket::new(ts_nanos + i as u64 * bucket_dur);
            }
            self.write_idx = 0;
            self.window_start = ts_nanos;
            return;
        }

        loop {
            let current_start = self.buckets[self.write_idx].ts_start;
            if ts_nanos < current_start + bucket_dur {
                break;
            }
            let next_idx = (self.write_idx + 1) % n;
            let next_start = current_start + bucket_dur;
            self.buckets[next_idx] = TemporalBucket::new(next_start);
            self.write_idx = next_idx;
            self.window_start = next_start - (n as u64 - 1) * bucket_dur;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TemporalAggregator — all six window classes
// ─────────────────────────────────────────────────────────────────────────────

/// Aggregates samples across all six window classes simultaneously.
pub struct TemporalAggregator {
    pub nano: CircularWindow,
    pub micro: CircularWindow,
    pub short: CircularWindow,
    pub medium: CircularWindow,
    pub long: CircularWindow,
    pub historical: CircularWindow,
}

impl TemporalAggregator {
    pub fn new(now_nanos: u64) -> Self {
        Self {
            nano: CircularWindow::new(WindowClass::Nano, now_nanos),
            micro: CircularWindow::new(WindowClass::Micro, now_nanos),
            short: CircularWindow::new(WindowClass::Short, now_nanos),
            medium: CircularWindow::new(WindowClass::Medium, now_nanos),
            long: CircularWindow::new(WindowClass::Long, now_nanos),
            historical: CircularWindow::new(WindowClass::Historical, now_nanos),
        }
    }

    /// Record a value at `ts_nanos` into ALL window classes.
    #[inline]
    pub fn record_all(&mut self, ts_nanos: u64, value: f64) {
        self.nano.record(ts_nanos, value);
        self.micro.record(ts_nanos, value);
        self.short.record(ts_nanos, value);
        self.medium.record(ts_nanos, value);
        self.long.record(ts_nanos, value);
        self.historical.record(ts_nanos, value);
    }

    pub fn aggregate(&self, class: WindowClass) -> TemporalBucket {
        match class {
            WindowClass::Nano => self.nano.aggregate(),
            WindowClass::Micro => self.micro.aggregate(),
            WindowClass::Short => self.short.aggregate(),
            WindowClass::Medium => self.medium.aggregate(),
            WindowClass::Long => self.long.aggregate(),
            WindowClass::Historical => self.historical.aggregate(),
        }
    }

    pub fn z_score(&self, class: WindowClass, value: f64) -> f64 {
        self.aggregate(class).z_score(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_welford_online() {
        let mut b = TemporalBucket::new(0);
        for v in [1.0, 2.0, 3.0, 4.0, 5.0] {
            b.update(v);
        }
        assert!((b.mean - 3.0).abs() < 1e-9);
        assert_eq!(b.count, 5);
        assert_eq!(b.max, 5.0);
        assert_eq!(b.min, 1.0);
    }

    #[test]
    fn circular_window_evicts() {
        let start = 1_000_000_000u64;
        let mut w = CircularWindow::new(WindowClass::Nano, start);
        w.record(start, 1.0);
        w.record(start + 10_000_000, 2.0);
        let agg = w.aggregate();
        assert!(agg.count > 0);
    }

    #[test]
    fn aggregator_all_windows() {
        let now = 1_000_000_000_000u64;
        let mut agg = TemporalAggregator::new(now);
        for i in 0..100 {
            agg.record_all(now + i * 1_000_000, i as f64);
        }
        let short = agg.aggregate(WindowClass::Short);
        assert!(short.count > 0);
        assert!(short.mean >= 0.0);
    }

    #[test]
    fn advance_to_does_not_loop_unboundedly_on_large_gap() {
        // Regression test for the runaway-loop bug this pass fixes: a
        // single event with a timestamp far ahead of the window (clock
        // skew, backfill, corrupted data) must not stall the caller.
        let start = 1_000_000_000u64;
        let mut w = CircularWindow::new(WindowClass::Nano, start); // 5ms window, 16 buckets
        w.record(start, 1.0);
        // Jump 1 hour ahead — the old bucket-by-bucket implementation
        // would need roughly 11 million iterations to reach this point.
        let far_future = start + 3_600_000_000_000;
        w.record(far_future, 2.0);
        let agg = w.aggregate();
        assert!(agg.count > 0);
    }

    #[test]
    fn advance_to_fast_path_resets_window_start_correctly() {
        let start = 1_000_000_000u64;
        let mut w = CircularWindow::new(WindowClass::Micro, start); // 50ms window, 16 buckets
        let jump_target = start + 10_000_000_000; // far beyond the 50ms span
        w.record(jump_target, 5.0);
        // After the fast-path reset, a sample recorded exactly at the
        // jump target must land in a fresh, non-stale bucket.
        let agg = w.aggregate();
        assert_eq!(agg.count, 1);
        assert_eq!(agg.mean, 5.0);
    }
}