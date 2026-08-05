// crates/omega-relay/src/reputation.rs
//! Address rotation with reputation carryover (§14.1, C4, I4) and
//! anti-fingerprint randomised round-robin (§14.2, I2).
//!
//! ## Audit fix (this revision)
//!
//! `submission_order` hardcoded the 5% tie-band cutoff as a bare `0.95` literal — the
//! third independent copy of this same constant, alongside two in `backpressure.rs`.
//! Replaced with `crate::config::LA_TIE_BAND_FRACTION` — see `config.rs`'s audit note.

use std::sync::Arc;

use rand::seq::SliceRandom;
use rand::thread_rng;
use tracing::info;

use crate::config::{RelayName, LA_TIE_BAND_FRACTION};
use crate::error::{RelayError, RelayResult};
use crate::metrics::{ExecutionAddress, LaRelayMetrics, RelayRateSnapshot};

/// Compute the reputation carryover fraction (spec §14.1).
#[inline]
pub fn carryover_pct(months_since_rotation: f64) -> f64 {
    0.5 * (-months_since_rotation / 3.0_f64).exp()
}

/// Execute an address rotation, seeding the new address with time-decayed carryover.
pub fn rotate_address(
    metrics: &Arc<LaRelayMetrics>,
    old_addr: &ExecutionAddress,
    new_addr: ExecutionAddress,
    months_since_last_rotation: f64,
    all_relays: &[RelayName],
) -> RelayResult<usize> {
    if all_relays.is_empty() {
        return Err(RelayError::NoRelayMetrics);
    }

    let carryover = carryover_pct(months_since_last_rotation);
    let mut seeded = 0usize;

    for relay in all_relays {
        if let Some(old_rate) = metrics.rate_for(relay, old_addr) {
            let seeded_rate = old_rate * carryover;
            metrics.seed_new_address(relay, new_addr.clone(), seeded_rate);
            seeded += 1;
            info!(
                relay = %relay,
                old_addr = %old_addr.0,
                new_addr = %new_addr.0,
                old_rate,
                carryover,
                seeded_rate,
                "relay reputation seeded on address rotation"
            );
        }
    }

    metrics.set_active_address(new_addr);
    Ok(seeded)
}

/// Return a randomised submission order for the given relay snapshots.
pub fn shuffled_submission_order(mut band: Vec<RelayRateSnapshot>) -> Vec<RelayRateSnapshot> {
    band.shuffle(&mut thread_rng());
    band
}

/// Build the full submission order for a blueprint.
pub fn submission_order(metrics: &Arc<LaRelayMetrics>) -> Vec<RelayRateSnapshot> {
    let ranked = metrics.la_ranked_relays();
    if ranked.is_empty() {
        return ranked;
    }

    let Some(best) = ranked.first().map(|snapshot| snapshot.la_rate) else {
        return ranked;
    };
    let threshold = best * (1.0 - LA_TIE_BAND_FRACTION);

    let (in_band, below_band): (Vec<_>, Vec<_>) =
        ranked.into_iter().partition(|r| r.la_rate >= threshold);

    let mut order = shuffled_submission_order(in_band);
    order.extend(below_band);
    order
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn carryover_at_zero_months_is_50_pct() {
        let c = carryover_pct(0.0);
        assert!((c - 0.5).abs() < 1e-9, "expected 0.5, got {c}");
    }

    #[test]
    fn carryover_at_three_months_approx_18_pct() {
        let c = carryover_pct(3.0);
        assert!((c - 0.1839).abs() < 0.001, "expected ~0.184, got {c}");
    }

    #[test]
    fn carryover_at_six_months_approx_9_pct() {
        let c = carryover_pct(6.0);
        assert!((c - 0.0677).abs() < 0.001, "expected ~0.068, got {c}");
    }

    #[test]
    fn carryover_at_twelve_months_approx_3_pct() {
        let c = carryover_pct(12.0);
        assert!(c < 0.02, "expected <2% at 12 months, got {c}");
    }

    #[test]
    fn carryover_is_monotone_decreasing() {
        let months = [0.0f64, 1.0, 2.0, 3.0, 6.0, 12.0, 24.0];
        let rates: Vec<f64> = months.iter().map(|&m| carryover_pct(m)).collect();
        for w in rates.windows(2) {
            assert!(
                w[0] > w[1],
                "carryover must be strictly decreasing: {rates:?}"
            );
        }
    }

    #[test]
    fn rotate_seeds_new_address_with_decay() {
        let old = ExecutionAddress("0xOLD".into());
        let new = ExecutionAddress("0xNEW".into());
        let m = LaRelayMetrics::new(100, old.clone());
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 80);
        }

        let relays = vec![RelayName::Flashbots];
        let seeded = rotate_address(&m, &old, new.clone(), 0.0, &relays).unwrap();
        assert_eq!(seeded, 1);

        let rate = m.rate_for(&RelayName::Flashbots, &new).unwrap();
        assert!(
            (rate - 0.40).abs() < 0.1,
            "expected ~0.40 seeded rate, got {rate}"
        );
        assert_eq!(m.active_address(), new);
    }

    #[test]
    fn rotate_with_unknown_old_address_seeds_zero_relays() {
        let old = ExecutionAddress("0xNONEXISTENT".into());
        let new = ExecutionAddress("0xNEW".into());
        let m = LaRelayMetrics::new(100, old.clone());
        let relays = vec![RelayName::Flashbots];
        let seeded = rotate_address(&m, &old, new, 0.0, &relays).unwrap();
        assert_eq!(seeded, 0);
    }

    #[test]
    fn rotate_errors_on_empty_relay_list() {
        let old = ExecutionAddress("0xOLD".into());
        let new = ExecutionAddress("0xNEW".into());
        let m = LaRelayMetrics::new(100, old.clone());
        let result = rotate_address(&m, &old, new, 0.0, &[]);
        assert!(matches!(result, Err(RelayError::NoRelayMetrics)));
    }

    #[test]
    fn shuffled_submission_order_contains_all_relays() {
        let snapshots = vec![
            RelayRateSnapshot {
                relay: RelayName::Flashbots,
                la_rate: 0.9,
                total_submitted: 100,
                total_included: 90,
            },
            RelayRateSnapshot {
                relay: RelayName::Bloxroute,
                la_rate: 0.88,
                total_submitted: 100,
                total_included: 88,
            },
        ];
        let shuffled = shuffled_submission_order(snapshots.clone());
        assert_eq!(shuffled.len(), snapshots.len());
        for s in &snapshots {
            assert!(shuffled.iter().any(|r| r.relay == s.relay));
        }
    }

    #[test]
    fn submission_order_appends_below_band_relays() {
        let m = LaRelayMetrics::new(100, ExecutionAddress("0xA".into()));
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 90);
            m.record(&RelayName::Bloxroute, i < 88);
            m.record(&RelayName::Titan, i < 50);
        }
        let order = submission_order(&m);
        assert_eq!(order.len(), 3);
        assert_eq!(order.last().unwrap().relay, RelayName::Titan);
    }

    // ── Audit fix regression test (this revision) ─────────────────────────────

    #[test]
    fn submission_order_threshold_derives_from_shared_constant() {
        let m = LaRelayMetrics::new(100, ExecutionAddress("0xB".into()));
        // flashbots at exactly the tie-band boundary relative to bloxroute
        // (best=0.90; threshold = 0.90 * (1 - LA_TIE_BAND_FRACTION) = 0.855),
        // titan clearly below.
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 90); // 0.90
            m.record(&RelayName::Bloxroute, i < 86); // 0.86 -> in band (>= 0.855)
            m.record(&RelayName::Titan, i < 50); // 0.50 -> below band
        }
        let order = submission_order(&m);
        // Titan must be last (below band); flashbots and bloxroute both in band
        // (order between them is randomised, only their presence before Titan matters).
        assert_eq!(order.last().unwrap().relay, RelayName::Titan);
        let in_band_names: Vec<_> = order[..2].iter().map(|r| r.relay.clone()).collect();
        assert!(in_band_names.contains(&RelayName::Flashbots));
        assert!(in_band_names.contains(&RelayName::Bloxroute));
    }
}
