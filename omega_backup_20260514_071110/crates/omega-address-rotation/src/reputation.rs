ï»¿// crates/omega-address-rotation/src/reputation.rs
//
// Relay reputation carryover with time-decay (spec Â§14.1, fix C4 + I4).
//
// ## Problem (fix C4)
//
//   In v11, a new execution address started with zero relay reputation.
//   No relay knew the new address, so it was consistently deprioritised
//   or rejected by relays that use address-based inclusion scoring.
//   This caused a multi-day warm-up penalty after every rotation.
//
// ## Solution (fix C4 + I4)
//
//   On address rotation, seed the new address with a fraction of the
//   old address's per-relay LA inclusion rate.  The seeded fraction
//   decays exponentially with the time since the last rotation so that
//   stale reputation data does not contaminate current estimates.
//
// ## Formula (Â§14.1 code block â€” authoritative)
//
//   carryover_pct = base_carryover Ã— exp(-months_since_rotation / decay_rate)
//
//   Default values (from RotationConfig):
//     base_carryover  = 0.50 (50% at rotation time)
//     decay_rate      = 3.0  (months; true half-life â‰ˆ 2.08 months)
//
//   Selected output values:
//     0 months  â†’ 50.0%
//     1 month   â†’ 35.8%   (spec table says 42% â€” table is approximate;
//                           formula is authoritative per Â§14.1 code block)
//     3 months  â†’ 18.4%
//     6 months  â†’  6.8%
//    12 months  â†’  0.9%
//
// ## Integration with LaRelayMetrics
//
//   `seed_relay_metrics` takes a snapshot of the old address's rolling-
//   window inclusion rates from `LaRelayMetrics` and seeds the new
//   address's metrics object with scaled synthetic outcomes.  The scaling
//   converts a fractional rate into a proportional number of synthetic
//   `true` outcomes inserted at the front of each relay's window.
//
//   Relay enumeration uses `ranked_relays(1.0, rng)` â€” a band_pct of 1.0
//   returns all registered relays, not just the top tie-band.

use chrono::{DateTime, Utc};
use omega_gas_war::LaRelayMetrics;
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// CarryoverParams
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Parameters for the reputation carryover formula (spec Â§14.1).
///
/// Passed as a struct so call sites don't have to repeat the three scalar
/// arguments, and so the type appears in the public API that `rotation.rs`
/// and `lib.rs` re-export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarryoverParams {
    /// Carryover fraction at the moment of rotation (default 0.50).
    pub base_carryover: f64,
    /// Exponential decay time constant in months (default 3.0).
    pub decay_rate_months: f64,
    /// Elapsed time since the previous rotation, in months.
    pub months_since_rotation: f64,
}

impl Default for CarryoverParams {
    fn default() -> Self {
        Self {
            base_carryover:        0.50,
            decay_rate_months:     3.0,
            months_since_rotation: 0.0,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Carryover formula
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute the reputation carryover fraction (spec Â§14.1, fix C4).
///
/// `months_since_rotation`: elapsed time since the last rotation, in months.
/// `base_carryover`: fraction at rotation time (default 0.50, from config).
/// `decay_rate`: time constant in months (default 3.0, from config).
///
/// Returns a value in [0.0, 1.0].  The result is clamped so that
/// floating-point edge cases never produce a value outside the valid range.
#[inline]
pub fn compute_carryover_pct(
    months_since_rotation: f64,
    base_carryover:        f64,
    decay_rate:            f64,
) -> f64 {
    if months_since_rotation < 0.0 || decay_rate <= 0.0 {
        return base_carryover.clamp(0.0, 1.0);
    }
    (base_carryover * (-months_since_rotation / decay_rate).exp()).clamp(0.0, 1.0)
}

/// Compute carryover from a `CarryoverParams` struct.
///
/// Delegates to `compute_carryover_pct` using the struct fields.
/// This is the entry point used by `AddressRotationManager::execute_rotation`.
#[inline]
pub fn compute_carryover_pct_params(params: &CarryoverParams) -> f64 {
    compute_carryover_pct(
        params.months_since_rotation,
        params.base_carryover,
        params.decay_rate_months,
    )
}

/// Convenience wrapper using the spec defaults (0.50 base, 3.0 decay).
///
/// Used in tests and where config is not yet available.
#[inline]
pub fn compute_carryover_pct_default(months_since_rotation: f64) -> f64 {
    compute_carryover_pct(months_since_rotation, 0.50, 3.0)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// RelayReputationEntry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-relay reputation entry for a single execution address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayReputationEntry {
    /// Relay identifier (matches the key in `LaRelayMetrics`).
    pub relay_name: String,

    /// LA inclusion rate [0.0, 1.0] for this (address, relay) pair.
    pub la_rate: f64,

    /// Number of samples backing this rate estimate.
    pub sample_count: usize,

    /// Whether this entry is a seeded estimate (true) or a measured value.
    pub is_seeded: bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ReputationSnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Full reputation snapshot for one execution address at one point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationSnapshot {
    /// Monotonic rotation sequence number (0 = initial address).
    pub rotation_index: u32,

    /// UTC timestamp of the rotation that produced this snapshot.
    pub rotated_at: DateTime<Utc>,

    /// Carryover fraction applied to produce this snapshot.
    pub carryover_pct: f64,

    /// Per-relay entries (one per registered relay).
    pub relays: Vec<RelayReputationEntry>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// seed_relay_metrics
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Seed a new `LaRelayMetrics` from an old one, scaled by `carryover_pct`.
///
/// ## Algorithm
///
/// For each relay registered in `old_metrics`:
///   1. Read the current LA inclusion rate from the old metrics.
///   2. Compute `seeded_rate = old_rate Ã— carryover_pct`.
///   3. Inject `seed_samples` synthetic outcomes into the new metrics:
///      - `included_count = round(seeded_rate Ã— seed_samples)` true outcomes
///      - `missed_count   = seed_samples - included_count` false outcomes
///
/// ## Relay enumeration
///
/// `LaRelayMetrics` does not expose a `relay_names()` iterator.  Instead,
/// `ranked_relays(1.0, rng)` with `band_pct = 1.0` returns all registered
/// relays (every relay is within 100% of the best rate), giving complete
/// enumeration without requiring a new public API on `LaRelayMetrics`.
///
/// ## `seed_samples`
///
/// Default: `min(window_size / 2, 20)`.  Passed explicitly by the caller
/// so the rotation manager can tune based on its window configuration.
pub fn seed_relay_metrics(
    old_metrics:   &LaRelayMetrics,
    new_metrics:   &LaRelayMetrics,
    carryover_pct: f64,
    seed_samples:  usize,
    rng:           &mut impl rand::Rng,
) {
    let carryover = carryover_pct.clamp(0.0, 1.0);

    // ranked_relays(1.0, rng) returns ALL registered relays â€” band_pct = 1.0
    // means every relay is within 100% of the best inclusion rate.
    // The rng is used internally for tie-breaking; any RNG is fine here.
    let relays = old_metrics.ranked_relays(1.0, rng);

    for entry in relays {
        // entry.relay_name: String, entry.la_rate: f64
        let old_rate    = entry.la_rate;
        let seeded_rate = old_rate * carryover;

        let included = (seeded_rate * seed_samples as f64).round() as usize;
        let missed   = seed_samples.saturating_sub(included);

        // Inject missed outcomes first so included outcomes are most recent
        for _ in 0..missed   { new_metrics.record(&entry.relay_name, false); }
        for _ in 0..included { new_metrics.record(&entry.relay_name, true);  }

        tracing::info!(
            relay        = %entry.relay_name,
            old_rate,
            carryover    = carryover_pct,
            seeded_rate,
            seed_samples,
            "Relay reputation seeded for new address",
        );
    }
}

/// Build a `ReputationSnapshot` from a `LaRelayMetrics` instance.
pub fn snapshot_from_metrics(
    metrics:        &LaRelayMetrics,
    rotation_index: u32,
    rotated_at:     DateTime<Utc>,
    carryover_pct:  f64,
    is_seeded:      bool,
    rng:            &mut impl rand::Rng,
) -> ReputationSnapshot {
    // band_pct = 1.0 â†’ all relays
    let relays = metrics
        .ranked_relays(1.0, rng)
        .into_iter()
        .map(|entry| RelayReputationEntry {
            la_rate:      entry.la_rate,
            sample_count: metrics.sample_count(&entry.relay_name),
            relay_name:   entry.relay_name,   // String â€” no conversion needed
            is_seeded,
        })
        .collect();

    ReputationSnapshot {
        rotation_index,
        rotated_at,
        carryover_pct,
        relays,
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    // â”€â”€ Formula correctness â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn carryover_at_zero_months_is_base() {
        let v = compute_carryover_pct(0.0, 0.50, 3.0);
        assert!((v - 0.50).abs() < 1e-9, "v={v}");
    }

    #[test]
    fn carryover_at_three_months() {
        // 0.50 Ã— exp(-1) â‰ˆ 0.1839
        let v = compute_carryover_pct(3.0, 0.50, 3.0);
        assert!((v - 0.1839).abs() < 0.001, "v={v}");
    }

    #[test]
    fn carryover_at_six_months() {
        // 0.50 Ã— exp(-2) â‰ˆ 0.0677
        let v = compute_carryover_pct(6.0, 0.50, 3.0);
        assert!((v - 0.0677).abs() < 0.001, "v={v}");
    }

    #[test]
    fn carryover_at_twelve_months() {
        // 0.50 Ã— exp(-4) â‰ˆ 0.00916
        let v = compute_carryover_pct(12.0, 0.50, 3.0);
        assert!((v - 0.00916).abs() < 0.001, "v={v}");
    }

    #[test]
    fn carryover_default_convenience_wrapper() {
        let v = compute_carryover_pct_default(0.0);
        assert!((v - 0.50).abs() < 1e-9);
    }

    #[test]
    fn carryover_params_struct_matches_scalar() {
        let params = CarryoverParams {
            base_carryover:        0.50,
            decay_rate_months:     3.0,
            months_since_rotation: 3.0,
        };
        let via_params = compute_carryover_pct_params(&params);
        let via_scalar = compute_carryover_pct(3.0, 0.50, 3.0);
        assert!((via_params - via_scalar).abs() < 1e-12);
    }

    #[test]
    fn carryover_negative_months_returns_base() {
        let v = compute_carryover_pct(-1.0, 0.50, 3.0);
        assert!((v - 0.50).abs() < 1e-9, "negative months must return base: v={v}");
    }

    #[test]
    fn carryover_zero_decay_rate_returns_base() {
        let v = compute_carryover_pct(6.0, 0.50, 0.0);
        assert!((v - 0.50).abs() < 1e-9, "zero decay rate must return base: v={v}");
    }

    #[test]
    fn carryover_always_in_range() {
        for (months, base, decay) in [
            (0.0, 0.5, 3.0), (100.0, 0.5, 3.0), (-5.0, 0.5, 3.0),
            (0.0, 1.0, 1.0), (0.0, 0.0, 3.0),
        ] {
            let v = compute_carryover_pct(months, base, decay);
            assert!(v >= 0.0 && v <= 1.0, "out of range: v={v} months={months}");
        }
    }

    // â”€â”€ Seeding â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn seed_relay_metrics_produces_expected_rate() {
        use omega_gas_war::{DEFAULT_WINDOW, LaRelayMetrics};
        use rand::SeedableRng;

        let old = LaRelayMetrics::new(DEFAULT_WINDOW);
        for _ in 0..20 { old.record("relay_a", true); }

        let new_m   = LaRelayMetrics::new(DEFAULT_WINDOW);
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        seed_relay_metrics(&old, &new_m, 0.50, 10, &mut rng);

        // seeded_rate = 1.0 Ã— 0.50 = 0.50 â†’ 5 included, 5 missed
        let rate = new_m.rate("relay_a");
        assert!((rate - 0.50).abs() < 0.15,
            "seeded rate should be ~50%: got {rate}");
    }

    #[test]
    fn seed_relay_metrics_zero_carryover_produces_all_misses() {
        use omega_gas_war::{DEFAULT_WINDOW, LaRelayMetrics};
        use rand::SeedableRng;

        let old = LaRelayMetrics::new(DEFAULT_WINDOW);
        for _ in 0..20 { old.record("relay_b", true); }

        let new_m   = LaRelayMetrics::new(DEFAULT_WINDOW);
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        seed_relay_metrics(&old, &new_m, 0.0, 10, &mut rng);

        let rate = new_m.rate("relay_b");
        assert!((rate - 0.0).abs() < 1e-9,
            "zero carryover must produce rate 0: {rate}");
    }
}