ï»¿// crates/omega-gas-war/src/relay_la_metrics.rs
// crates/omega-gas-war/src/relay_la_metrics.rs
//
// Per-relay LA inclusion-rate tracking (spec Â§11.2, Â§14).
//
// The cascade submission order is determined by each relay's recent LA
// inclusion rate â€” the fraction of submitted LA bundles that were
// included on-chain within the LA window (Â§11).  Relays within 5% of
// the best rate form a tie band; the submission order within the band is
// randomised each blueprint to prevent fingerprinting (fix I2, Â§11.2).
//
// ## Data model
//
//   Per relay: a bounded rolling window of boolean outcomes (included =
//   true / not included = false).  Window length is configurable; default
//   100 samples provides a responsive estimate without excessive noise.
//
//   DashMap<relay_name, RollingWindow> allows concurrent recording from
//   the relay submission loop without blocking the ranking query.
//
// ## Thread safety
//
//   `record()` and `ranked_relays()` both use DashMap entry APIs.
//   `record()` acquires a write entry per relay; `ranked_relays()`
//   acquires read entries.  The two operations never hold entries
//   simultaneously, so there is no deadlock risk.
//
// ## Decay / staleness
//
//   The rolling window inherently decays old observations by evicting
//   them.  No explicit time-decay is applied here â€” stale relays (no
//   activity for > N blocks) naturally show a 0.0 rate if their window
//   fills with misses.  If a relay has fewer than `min_sample_count`
//   observations it is assigned a neutral rate of 0.5 (optimistic prior)
//   rather than 0.0 to prevent cold-start penalisation.

use std::collections::VecDeque;
use std::sync::Arc;

use dashmap::DashMap;
use rand::seq::SliceRandom;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Default rolling window length (number of submission outcomes retained).
pub const DEFAULT_WINDOW: usize = 100;

/// Minimum samples before a relay's rate is trusted over the prior.
/// Below this count, `neutral_prior` is returned instead.
pub const MIN_SAMPLE_COUNT: usize = 5;

/// Neutral prior rate for relays with fewer than `MIN_SAMPLE_COUNT` samples.
///
/// 0.5 = optimistic: new relays are treated as having 50% inclusion rate
/// rather than 0% (which would permanently exclude them from the cascade).
pub const NEUTRAL_PRIOR: f64 = 0.5;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RollingWindow
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Bounded FIFO window of boolean LA bundle outcomes.
#[derive(Debug)]
struct RollingWindow {
    outcomes:  VecDeque<bool>,
    max_len:   usize,
    /// Cached count of `true` entries for O(1) rate computation.
    included:  usize,
}

impl RollingWindow {
    fn new(max_len: usize) -> Self {
        Self {
            outcomes: VecDeque::with_capacity(max_len),
            max_len,
            included: 0,
        }
    }

    /// Record one outcome.  Evicts the oldest entry when the window is full.
    fn push(&mut self, was_included: bool) {
        if self.outcomes.len() == self.max_len {
            if let Some(old) = self.outcomes.pop_front() {
                if old { self.included -= 1; }
            }
        }
        self.outcomes.push_back(was_included);
        if was_included { self.included += 1; }
    }

    /// Inclusion rate in [0.0, 1.0].  Returns `NEUTRAL_PRIOR` when
    /// fewer than `MIN_SAMPLE_COUNT` outcomes have been recorded.
    fn rate(&self) -> f64 {
        let n = self.outcomes.len();
        if n < MIN_SAMPLE_COUNT {
            return NEUTRAL_PRIOR;
        }
        self.included as f64 / n as f64
    }

    fn sample_count(&self) -> usize {
        self.outcomes.len()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RelayRank
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A relay's computed inclusion rate and ordering metadata.
#[derive(Debug, Clone)]
pub struct RelayRank {
    /// Relay identifier (matches the string used at registration).
    pub relay_name:    String,
    /// Recent LA inclusion rate [0.0, 1.0].
    pub la_rate:       f64,
    /// Number of samples in the rolling window.
    pub sample_count:  usize,
    /// Whether this relay falls within the tie band.
    pub in_tie_band:   bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LaRelayMetrics
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Concurrent per-relay LA inclusion-rate tracker.
///
/// Shared via `Arc<LaRelayMetrics>` between the relay submission loop
/// (calls `record`) and the Gas War Engine (calls `ranked_relays`).
#[derive(Debug)]
pub struct LaRelayMetrics {
    windows:     DashMap<String, RollingWindow>,
    window_size: usize,
}

impl LaRelayMetrics {
    /// Create a new metrics tracker.
    ///
    /// `window_size` is the number of outcomes retained per relay.
    /// Use `DEFAULT_WINDOW` for production.
    pub fn new(window_size: usize) -> Arc<Self> {
        Arc::new(Self {
            windows:     DashMap::new(),
            window_size: window_size.max(1),
        })
    }

    /// Record the outcome of a single LA bundle submission.
    ///
    /// `relay_name` is the relay identifier.  `was_included` is `true`
    /// when the bundle was included on-chain within the LA window.
    ///
    /// Creates an entry for `relay_name` if it does not yet exist.
    pub fn record(&self, relay_name: &str, was_included: bool) {
        self.windows
            .entry(relay_name.to_owned())
            .or_insert_with(|| RollingWindow::new(self.window_size))
            .push(was_included);

        tracing::debug!(
            relay        = relay_name,
            was_included,
            new_rate     = self.rate(relay_name),
            "LA inclusion outcome recorded",
        );
    }

    /// Current inclusion rate for a relay [0.0, 1.0].
    ///
    /// Returns `NEUTRAL_PRIOR` for unknown relays or those with fewer
    /// than `MIN_SAMPLE_COUNT` observations.
    pub fn rate(&self, relay_name: &str) -> f64 {
        self.windows
            .get(relay_name)
            .map(|w| w.rate())
            .unwrap_or(NEUTRAL_PRIOR)
    }

    /// Number of recorded samples for a relay.
    pub fn sample_count(&self, relay_name: &str) -> usize {
        self.windows
            .get(relay_name)
            .map(|w| w.sample_count())
            .unwrap_or(0)
    }

    /// Return relays ranked by LA inclusion rate, with tie-band
    /// randomisation (spec Â§11.2, fix I2).
    ///
    /// ## Ranking algorithm
    ///
    /// 1. Compute the rate for all registered relays.
    /// 2. Sort descending by rate (best relay first).
    /// 3. Mark relays within `tie_band_fraction` of the best rate as
    ///    in-tie-band.
    /// 4. Shuffle the in-tie-band subset using `rng` (anti-fingerprinting).
    /// 5. Return: shuffled tie-band relays first, then remaining relays
    ///    in descending rate order.
    ///
    /// ## Arguments
    ///
    /// - `tie_band_fraction`: relays with rate â‰¥ best_rate Ã— (1 - fraction)
    ///   are in the tie band.  Use `config.relay.inclusion_rate_tie_band_fraction`
    ///   (default 0.05).
    /// - `rng`: caller-supplied RNG.  Accept `&mut impl rand::Rng` so the
    ///   caller controls seeding (deterministic in tests, OS-seeded in prod).
    pub fn ranked_relays(
        &self,
        tie_band_fraction: f64,
        rng:               &mut impl rand::Rng,
    ) -> Vec<RelayRank> {
        // â”€â”€ Collect all rates â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        let mut ranks: Vec<RelayRank> = self.windows
            .iter()
            .map(|entry| RelayRank {
                relay_name:   entry.key().clone(),
                la_rate:      entry.value().rate(),
                sample_count: entry.value().sample_count(),
                in_tie_band:  false, // set below
            })
            .collect();

        if ranks.is_empty() {
            return ranks;
        }

        // â”€â”€ Sort descending â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        ranks.sort_by(|a, b| {
            b.la_rate
                .partial_cmp(&a.la_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let best_rate = ranks[0].la_rate;
        let threshold = best_rate * (1.0 - tie_band_fraction.clamp(0.0, 1.0));

        // â”€â”€ Mark tie band â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        for rank in &mut ranks {
            rank.in_tie_band = rank.la_rate >= threshold;
        }

        // â”€â”€ Separate and shuffle tie band â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        let tie_count = ranks.iter().filter(|r| r.in_tie_band).count();
        if tie_count > 1 {
            ranks[..tie_count].shuffle(rng);
        }

        ranks
    }

    /// Number of relays currently tracked.
    pub fn relay_count(&self) -> usize {
        self.windows.len()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn metrics() -> Arc<LaRelayMetrics> {
        LaRelayMetrics::new(DEFAULT_WINDOW)
    }

    fn seeded_rng() -> impl rand::Rng {
        rand::rngs::StdRng::seed_from_u64(42)
    }

    // â”€â”€ record / rate â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn unknown_relay_returns_neutral_prior() {
        let m = metrics();
        assert!((m.rate("nonexistent") - NEUTRAL_PRIOR).abs() < 1e-9);
    }

    #[test]
    fn rate_after_all_included() {
        let m = metrics();
        for _ in 0..MIN_SAMPLE_COUNT {
            m.record("relay_a", true);
        }
        assert!((m.rate("relay_a") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rate_after_all_missed() {
        let m = metrics();
        for _ in 0..MIN_SAMPLE_COUNT {
            m.record("relay_a", false);
        }
        assert!((m.rate("relay_a") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn rate_is_neutral_prior_below_min_sample_count() {
        let m = metrics();
        for _ in 0..(MIN_SAMPLE_COUNT - 1) {
            m.record("relay_a", true);
        }
        assert!(
            (m.rate("relay_a") - NEUTRAL_PRIOR).abs() < 1e-9,
            "below min samples must return neutral prior",
        );
    }

    #[test]
    fn rate_50_percent_mixed() {
        let m = metrics();
        for i in 0..20 {
            m.record("relay_a", i % 2 == 0);
        }
        assert!((m.rate("relay_a") - 0.5).abs() < 1e-9);
    }

    // â”€â”€ rolling window eviction â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn window_evicts_oldest_entry() {
        let m = LaRelayMetrics::new(5);
        // Fill with 5 misses
        for _ in 0..5 { m.record("relay_a", false); }
        assert!((m.rate("relay_a") - 0.0).abs() < 1e-9);
        // Add 5 hits â€” evicts the 5 misses
        for _ in 0..5 { m.record("relay_a", true); }
        assert!((m.rate("relay_a") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rolling_window_cached_count_stays_correct() {
        let m = LaRelayMetrics::new(4);
        // Push pattern: T T F F â†’ rate 0.5
        for &b in &[true, true, false, false] { m.record("r", b); }
        assert!((m.rate("r") - 0.5).abs() < 1e-9);
        // Push F â†’ evicts first T â†’ T F F F â†’ rate 0.25
        m.record("r", false);
        assert!((m.rate("r") - 0.25).abs() < 1e-9);
    }

    // â”€â”€ ranked_relays â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn ranked_relays_empty_returns_empty() {
        let m = metrics();
        let mut rng = seeded_rng();
        assert!(m.ranked_relays(0.05, &mut rng).is_empty());
    }

    #[test]
    fn ranked_relays_sorted_descending() {
        let m = metrics();
        // relay_a: 10/10 = 1.0
        for _ in 0..10 { m.record("relay_a", true); }
        // relay_b: 5/10 = 0.5
        for i in 0..10 { m.record("relay_b", i < 5); }
        // relay_c: 0/10 = 0.0
        for _ in 0..10 { m.record("relay_c", false); }

        let mut rng = seeded_rng();
        let ranked  = m.ranked_relays(0.05, &mut rng);
        assert_eq!(ranked.len(), 3);

        // relay_a must be first (rate 1.0), relay_c must be last (rate 0.0)
        assert_eq!(ranked[0].relay_name, "relay_a");
        assert_eq!(ranked[2].relay_name, "relay_c");

        // Rates must be non-increasing
        for i in 1..ranked.len() {
            assert!(ranked[i - 1].la_rate >= ranked[i].la_rate,
                "ranks must be non-increasing: {:?}", ranked);
        }
    }

    #[test]
    fn tie_band_marked_correctly() {
        let m = metrics();
        // relay_a and relay_b both at 1.0 â†’ both in tie band at 5%
        for _ in 0..10 { m.record("relay_a", true); }
        for _ in 0..10 { m.record("relay_b", true); }
        // relay_c at 0.0 â†’ not in tie band
        for _ in 0..10 { m.record("relay_c", false); }

        let mut rng   = seeded_rng();
        let ranked    = m.ranked_relays(0.05, &mut rng);
        let tie_count = ranked.iter().filter(|r| r.in_tie_band).count();
        assert_eq!(tie_count, 2, "relay_a and relay_b must both be in tie band");

        let non_tie: Vec<_> = ranked.iter().filter(|r| !r.in_tie_band).collect();
        assert_eq!(non_tie.len(), 1);
        assert_eq!(non_tie[0].relay_name, "relay_c");
    }

    #[test]
    fn single_relay_always_in_tie_band() {
        let m = metrics();
        for _ in 0..10 { m.record("relay_a", true); }
        let mut rng = seeded_rng();
        let ranked  = m.ranked_relays(0.05, &mut rng);
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].in_tie_band);
    }
}