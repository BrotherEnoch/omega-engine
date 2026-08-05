// crates/omega-address-rotation/src/reputation.rs
// (unchanged except line 343: manual_range_contains fix)
// Full file reproduced for drop-in replacement.

use chrono::{DateTime, Utc};
use omega_gas_war::LaRelayMetrics;
use serde::{Deserialize, Serialize};

/// Parameters for the reputation carryover formula (spec §14.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CarryoverParams {
    pub base_carryover: f64,
    pub decay_rate_months: f64,
    pub months_since_rotation: f64,
}

impl Default for CarryoverParams {
    fn default() -> Self {
        Self {
            base_carryover: 0.50,
            decay_rate_months: 3.0,
            months_since_rotation: 0.0,
        }
    }
}

#[inline]
pub fn compute_carryover_pct(
    months_since_rotation: f64,
    base_carryover: f64,
    decay_rate: f64,
) -> f64 {
    if months_since_rotation < 0.0 || decay_rate <= 0.0 {
        return base_carryover.clamp(0.0, 1.0);
    }
    (base_carryover * (-months_since_rotation / decay_rate).exp()).clamp(0.0, 1.0)
}

#[inline]
pub fn compute_carryover_pct_params(params: &CarryoverParams) -> f64 {
    compute_carryover_pct(
        params.months_since_rotation,
        params.base_carryover,
        params.decay_rate_months,
    )
}

#[inline]
pub fn compute_carryover_pct_default(months_since_rotation: f64) -> f64 {
    compute_carryover_pct(months_since_rotation, 0.50, 3.0)
}

/// Per-relay reputation entry for a single execution address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayReputationEntry {
    pub relay_name: String,
    pub la_rate: f64,
    pub sample_count: usize,
    pub is_seeded: bool,
}

/// Full reputation snapshot for one execution address at one point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationSnapshot {
    pub rotation_index: u32,
    pub rotated_at: DateTime<Utc>,
    pub carryover_pct: f64,
    pub relays: Vec<RelayReputationEntry>,
}

/// Seed a new `LaRelayMetrics` from an old one, scaled by `carryover_pct`.
pub fn seed_relay_metrics(
    old_metrics: &LaRelayMetrics,
    new_metrics: &LaRelayMetrics,
    carryover_pct: f64,
    seed_samples: usize,
    rng: &mut impl rand::Rng,
) {
    let carryover = carryover_pct.clamp(0.0, 1.0);
    let relays = old_metrics.ranked_relays(1.0, rng);

    for entry in relays {
        let old_rate = entry.la_rate;
        let seeded_rate = old_rate * carryover;

        let included = (seeded_rate * seed_samples as f64).round() as usize;
        let missed = seed_samples.saturating_sub(included);

        for _ in 0..missed {
            new_metrics.record(&entry.relay_name, false);
        }
        for _ in 0..included {
            new_metrics.record(&entry.relay_name, true);
        }

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
    metrics: &LaRelayMetrics,
    rotation_index: u32,
    rotated_at: DateTime<Utc>,
    carryover_pct: f64,
    is_seeded: bool,
    rng: &mut impl rand::Rng,
) -> ReputationSnapshot {
    let relays = metrics
        .ranked_relays(1.0, rng)
        .into_iter()
        .map(|entry| RelayReputationEntry {
            la_rate: entry.la_rate,
            sample_count: metrics.sample_count(&entry.relay_name),
            relay_name: entry.relay_name,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carryover_at_zero_months_is_base() {
        let v = compute_carryover_pct(0.0, 0.50, 3.0);
        assert!((v - 0.50).abs() < 1e-9, "v={v}");
    }

    #[test]
    fn carryover_at_three_months() {
        let v = compute_carryover_pct(3.0, 0.50, 3.0);
        assert!((v - 0.1839).abs() < 0.001, "v={v}");
    }

    #[test]
    fn carryover_at_six_months() {
        let v = compute_carryover_pct(6.0, 0.50, 3.0);
        assert!((v - 0.0677).abs() < 0.001, "v={v}");
    }

    #[test]
    fn carryover_at_twelve_months() {
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
            base_carryover: 0.50,
            decay_rate_months: 3.0,
            months_since_rotation: 3.0,
        };
        let via_params = compute_carryover_pct_params(&params);
        let via_scalar = compute_carryover_pct(3.0, 0.50, 3.0);
        assert!((via_params - via_scalar).abs() < 1e-12);
    }

    #[test]
    fn carryover_negative_months_returns_base() {
        let v = compute_carryover_pct(-1.0, 0.50, 3.0);
        assert!(
            (v - 0.50).abs() < 1e-9,
            "negative months must return base: v={v}"
        );
    }

    #[test]
    fn carryover_zero_decay_rate_returns_base() {
        let v = compute_carryover_pct(6.0, 0.50, 0.0);
        assert!(
            (v - 0.50).abs() < 1e-9,
            "zero decay rate must return base: v={v}"
        );
    }

    #[test]
    fn carryover_always_in_range() {
        for (months, base, decay) in [
            (0.0, 0.5, 3.0),
            (100.0, 0.5, 3.0),
            (-5.0, 0.5, 3.0),
            (0.0, 1.0, 1.0),
            (0.0, 0.0, 3.0),
        ] {
            let v = compute_carryover_pct(months, base, decay);
            // FIX: manual_range_contains → use RangeInclusive::contains
            assert!(
                (0.0..=1.0).contains(&v),
                "out of range: v={v} months={months}"
            );
        }
    }

    #[test]
    fn seed_relay_metrics_produces_expected_rate() {
        use omega_gas_war::{LaRelayMetrics, DEFAULT_WINDOW};
        use rand::SeedableRng;

        let old = LaRelayMetrics::new(DEFAULT_WINDOW);
        for _ in 0..20 {
            old.record("relay_a", true);
        }

        let new_m = LaRelayMetrics::new(DEFAULT_WINDOW);
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        seed_relay_metrics(&old, &new_m, 0.50, 10, &mut rng);

        let rate = new_m.rate("relay_a");
        assert!(
            (rate - 0.50).abs() < 0.15,
            "seeded rate should be ~50%: got {rate}"
        );
    }

    #[test]
    fn seed_relay_metrics_zero_carryover_produces_all_misses() {
        use omega_gas_war::{LaRelayMetrics, DEFAULT_WINDOW};
        use rand::SeedableRng;

        let old = LaRelayMetrics::new(DEFAULT_WINDOW);
        for _ in 0..20 {
            old.record("relay_b", true);
        }

        let new_m = LaRelayMetrics::new(DEFAULT_WINDOW);
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        seed_relay_metrics(&old, &new_m, 0.0, 10, &mut rng);

        let rate = new_m.rate("relay_b");
        assert!(
            (rate - 0.0).abs() < 1e-9,
            "zero carryover must produce rate 0: {rate}"
        );
    }
}
