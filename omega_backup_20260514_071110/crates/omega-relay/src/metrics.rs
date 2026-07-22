// crates/omega-relay/src/metrics.rs
//! Per-relay LA inclusion-rate metrics.
//!
//! This module owns the live `LaRelayMetrics` struct that every other module
//! reads to decide submission ordering, reputation carryover, and cascade
//! tie-band selection.  All writes are lock-free via `DashMap`; reads are
//! wait-free.
//!
//! ## Spec references
//! - Â§11.2  Cascade ordering uses `la_ranked_relays()`
//! - Â§14.1  Address rotation reads per-relay `la_inclusion_rate` per address
//! - Â§14.2  Tie band = relays within 5% of best inclusion rate

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::config::RelayName;

// â”€â”€ Types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A hex-encoded Ethereum address used as the execution address key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionAddress(pub String);

/// Rolling window for inclusion rate calculation.
/// Uses a fixed-size circular buffer of the last N submission outcomes.
#[derive(Debug)]
struct RollingWindow {
    window: Vec<bool>, // true = included, false = not included
    cursor: usize,
    filled: usize,
    capacity: usize,
}

impl RollingWindow {
    fn new(capacity: usize) -> Self {
        Self {
            window: vec![false; capacity],
            cursor: 0,
            filled: 0,
            capacity,
        }
    }

    fn record(&mut self, included: bool) {
        self.window[self.cursor] = included;
        self.cursor = (self.cursor + 1) % self.capacity;
        if self.filled < self.capacity {
            self.filled += 1;
        }
    }

    /// Inclusion rate in [0.0, 1.0].  Returns 0.0 until at least one sample.
    fn rate(&self) -> f64 {
        if self.filled == 0 {
            return 0.0;
        }
        let included = self.window[..self.filled].iter().filter(|&&b| b).count();
        included as f64 / self.filled as f64
    }
}

/// Per-(relay, address) metrics.
#[derive(Debug)]
struct RelayAddressMetrics {
    window: RollingWindow,
    last_updated: Instant,
    total_submitted: u64,
    total_included: u64,
}

impl RelayAddressMetrics {
    fn new(window_size: usize) -> Self {
        Self {
            window: RollingWindow::new(window_size),
            last_updated: Instant::now(),
            total_submitted: 0,
            total_included: 0,
        }
    }

    fn record_submission(&mut self, included: bool) {
        self.window.record(included);
        self.total_submitted += 1;
        if included {
            self.total_included += 1;
        }
        self.last_updated = Instant::now();
    }

    fn la_rate(&self) -> f64 {
        self.window.rate()
    }
}

// â”€â”€ Public snapshot â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Snapshot of a relay's current LA inclusion rate â€” used for cascade ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRateSnapshot {
    pub relay: RelayName,
    /// Rolling-window LA inclusion rate in [0.0, 1.0].
    pub la_rate: f64,
    pub total_submitted: u64,
    pub total_included: u64,
}

// â”€â”€ LaRelayMetrics â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Live per-relay LA inclusion-rate store.
///
/// Keyed by `(RelayName, ExecutionAddress)` so reputation is address-scoped
/// (required for carryover seeding in Â§14.1).
pub struct LaRelayMetrics {
    /// `(relay_name, address)` â†’ metrics
    inner: DashMap<(RelayName, ExecutionAddress), RwLock<RelayAddressMetrics>>,
    /// Rolling-window size (number of submissions to track per key).
    window_size: usize,
    /// Active execution address â€” updated on rotation.
    active_address: RwLock<ExecutionAddress>,
}

impl LaRelayMetrics {
    pub fn new(window_size: usize, initial_address: ExecutionAddress) -> Arc<Self> {
        Arc::new(Self {
            inner: DashMap::new(),
            window_size,
            active_address: RwLock::new(initial_address),
        })
    }

    /// Record a bundle outcome for (relay, current active address).
    pub fn record(&self, relay: &RelayName, included: bool) {
        let addr = self.active_address.read().clone();
        let key = (relay.clone(), addr);
        let entry = self
            .inner
            .entry(key)
            .or_insert_with(|| RwLock::new(RelayAddressMetrics::new(self.window_size)));
        entry.write().record_submission(included);
    }

    /// Seed a new address with a pre-computed rate (used during address rotation Â§14.1).
    pub fn seed_new_address(&self, relay: &RelayName, new_addr: ExecutionAddress, seeded_rate: f64) {
        let key = (relay.clone(), new_addr);
        let mut metrics = RelayAddressMetrics::new(self.window_size);
        // Pre-fill window with synthetic samples matching the seeded rate so
        // the rolling average starts at seeded_rate rather than 0.0.
        let filled = self.window_size.min(50); // seed with up to 50 synthetic samples
        let inclusions = (filled as f64 * seeded_rate.clamp(0.0, 1.0)).round() as usize;
        for i in 0..filled {
            metrics.window.record(i < inclusions);
        }
        metrics.window.filled = filled;
        self.inner.insert(key, RwLock::new(metrics));
    }

    /// Update the active execution address.
    pub fn set_active_address(&self, addr: ExecutionAddress) {
        *self.active_address.write() = addr;
    }

    /// Current active address.
    pub fn active_address(&self) -> ExecutionAddress {
        self.active_address.read().clone()
    }

    /// Ranked relay snapshots for the active address, best inclusion rate first.
    /// Used by cascade submission (Â§11.2) and non-cascade normal LA (Â§14.2).
    pub fn la_ranked_relays(&self) -> Vec<RelayRateSnapshot> {
        let addr = self.active_address.read().clone();
        let mut snapshots: Vec<RelayRateSnapshot> = self
            .inner
            .iter()
            .filter(|entry| entry.key().1 == addr)
            .map(|entry| {
                let m = entry.value().read();
                RelayRateSnapshot {
                    relay: entry.key().0.clone(),
                    la_rate: m.la_rate(),
                    total_submitted: m.total_submitted,
                    total_included: m.total_included,
                }
            })
            .collect();

        snapshots.sort_by(|a, b| b.la_rate.partial_cmp(&a.la_rate).unwrap_or(std::cmp::Ordering::Equal));
        snapshots
    }

    /// Relays within `tie_pct` (e.g. 0.05 = 5%) of the best inclusion rate.
    /// These form the tie band for randomized round-robin (Â§11.2, Â§14.2).
    pub fn tie_band(&self, tie_pct: f64) -> Vec<RelayRateSnapshot> {
        let ranked = self.la_ranked_relays();
        if ranked.is_empty() {
            return ranked;
        }
        let best = ranked[0].la_rate;
        let threshold = best * (1.0 - tie_pct);
        ranked.into_iter().filter(|r| r.la_rate >= threshold).collect()
    }

    /// Raw inclusion rate for a specific (relay, address) â€” used by reputation carryover.
    pub fn rate_for(&self, relay: &RelayName, addr: &ExecutionAddress) -> Option<f64> {
        let key = (relay.clone(), addr.clone());
        self.inner.get(&key).map(|m| m.read().la_rate())
    }

    /// Age of the last update for (relay, active_address), used by staleness checks.
    pub fn last_updated_age(&self, relay: &RelayName) -> Option<Duration> {
        let addr = self.active_address.read().clone();
        let key = (relay.clone(), addr);
        self.inner.get(&key).map(|m| m.read().last_updated.elapsed())
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metrics() -> Arc<LaRelayMetrics> {
        LaRelayMetrics::new(100, ExecutionAddress("0xABCD".into()))
    }

    #[test]
    fn records_and_ranks() {
        let m = make_metrics();
        // flashbots: 8/10 = 0.8
        for i in 0..10 {
            m.record(&RelayName::Flashbots, i < 8);
        }
        // bloxroute: 3/10 = 0.3
        for i in 0..10 {
            m.record(&RelayName::Bloxroute, i < 3);
        }
        let ranked = m.la_ranked_relays();
        assert_eq!(ranked[0].relay, RelayName::Flashbots);
        assert!(ranked[0].la_rate > ranked[1].la_rate);
    }

    #[test]
    fn tie_band_filters_correctly() {
        let m = make_metrics();
        // flashbots: 0.90, bloxroute: 0.88, titan: 0.50
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 90);
            m.record(&RelayName::Bloxroute, i < 88);
            m.record(&RelayName::Titan, i < 50);
        }
        // 5% tie band: threshold = 0.90 * 0.95 = 0.855
        // flashbots (0.90) and bloxroute (0.88) are in; titan (0.50) is out
        let band = m.tie_band(0.05);
        assert_eq!(band.len(), 2, "titan must be excluded from tie band");
        let names: Vec<_> = band.iter().map(|r| &r.relay).collect();
        assert!(names.contains(&&RelayName::Flashbots));
        assert!(names.contains(&&RelayName::Bloxroute));
    }

    #[test]
    fn seed_new_address_sets_approximate_rate() {
        let m = make_metrics();
        m.seed_new_address(
            &RelayName::Flashbots,
            ExecutionAddress("0xNEW".into()),
            0.75,
        );
        m.set_active_address(ExecutionAddress("0xNEW".into()));
        let ranked = m.la_ranked_relays();
        assert!(!ranked.is_empty());
        let rate = ranked[0].la_rate;
        assert!(
            (rate - 0.75).abs() < 0.05,
            "seeded rate should be ~0.75, got {rate}"
        );
    }

    #[test]
    fn rate_starts_at_zero_for_unknown_address() {
        let m = make_metrics();
        let rate = m.rate_for(&RelayName::Eden, &ExecutionAddress("0xUNKNOWN".into()));
        assert!(rate.is_none());
    }
}