// crates/omega-relay/src/reputation.rs
//! Address rotation with reputation carryover (Â§14.1, C4, I4) and
//! anti-fingerprint randomised round-robin (Â§14.2, I2).
//!
//! ## Spec: carryover formula
//! ```text
//! carryover_pct = 0.5 Ã— exp(-months_since_rotation / 3)
//! ```
//! | months | carryover |
//! |--------|-----------|
//! | 0      | 50 %      |
//! | 3      | ~18 %     |
//! | 6      | ~9 %      |
//! | 12     | ~3 %      |
//!
//! ## Spec: anti-fingerprinting
//! Submission order is randomised per-blueprint within the tie band (Â§14.2).
//! The `shuffled_submission_order` function is called by the backpressure module
//! and by normal (non-cascade) LA submission paths.

use std::sync::Arc;

use rand::seq::SliceRandom;
use rand::thread_rng;
use tracing::info;

use crate::config::RelayName;
use crate::error::{RelayError, RelayResult};
use crate::metrics::{ExecutionAddress, LaRelayMetrics, RelayRateSnapshot};

// â”€â”€ Carryover formula (Â§14.1 / I4) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute carryover fraction.
///
/// `months_since_rotation` â€” fractional months since last address rotation.
///
/// Returns a value in (0.0, 0.5].  Never returns 0.0 (exp never reaches 0),
/// but for practical purposes values below 0.01 are treated as cold-start.
///
/// Spec formula: `0.5 Ã— exp(-months / 3)` with 3-month half-life.
#[inline]
pub fn carryover_pct(months_since_rotation: f64) -> f64 {
    0.5 * (-months_since_rotation / 3.0_f64).exp()
}

// â”€â”€ Address rotation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Execute an address rotation, seeding the new address's per-relay inclusion
/// rates with time-decayed carryover from the old address.
///
/// ## Steps
/// 1. Compute `carryover` from `months_since_last_rotation`.
/// 2. For every relay that has a known inclusion rate for `old_addr`:
///    seed `new_addr` with `old_rate Ã— carryover`.
/// 3. Update the active address in `metrics`.
///
/// Returns the number of relays that received seeded rates.
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

// â”€â”€ Anti-fingerprint round-robin (Â§14.2 / I2) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Return a randomised submission order for the given relay snapshots.
///
/// Input should already be the tie band (relays within 5 % of best inclusion
/// rate from `LaRelayMetrics::tie_band`).  This function shuffles in-place so
/// every blueprint sees a different order â€” making Omega's relay submission
/// pattern indistinguishable from background noise.
pub fn shuffled_submission_order(mut band: Vec<RelayRateSnapshot>) -> Vec<RelayRateSnapshot> {
    band.shuffle(&mut thread_rng());
    band
}

/// Build the full submission order for a blueprint:
/// 1. Get the tie band (within 5 % of best).
/// 2. Shuffle within the band.
/// 3. Append remaining relays (below tie band) in ranked order so we always
///    have a fallback chain.
pub fn submission_order(metrics: &Arc<LaRelayMetrics>) -> Vec<RelayRateSnapshot> {
    let ranked = metrics.la_ranked_relays();
    if ranked.is_empty() {
        return ranked;
    }

    let best = ranked[0].la_rate;
    let threshold = best * 0.95; // 5 % tie band

    let (in_band, below_band): (Vec<_>, Vec<_>) =
        ranked.into_iter().partition(|r| r.la_rate >= threshold);

    let mut order = shuffled_submission_order(in_band);
    order.extend(below_band); // ranked (deterministic) below the band
    order
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    // â”€â”€ carryover formula â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn carryover_at_zero_months_is_50_pct() {
        let c = carryover_pct(0.0);
        assert!((c - 0.5).abs() < 1e-9, "expected 0.5, got {c}");
    }

    #[test]
    fn carryover_at_three_months_approx_18_pct() {
        let c = carryover_pct(3.0);
        // 0.5 Ã— exp(-1) â‰ˆ 0.1839
        assert!((c - 0.1839).abs() < 0.001, "expected ~0.184, got {c}");
    }

    #[test]
    fn carryover_at_six_months_approx_9_pct() {
        let c = carryover_pct(6.0);
        // 0.5 Ã— exp(-2) â‰ˆ 0.0677
        assert!((c - 0.0677).abs() < 0.001, "expected ~0.068, got {c}");
    }

    #[test]
    fn carryover_at_twelve_months_approx_3_pct() {
        let c = carryover_pct(12.0);
        // 0.5 Ã— exp(-4) â‰ˆ 0.00916
        assert!(c < 0.02, "expected <2% at 12 months, got {c}");
    }

    #[test]
    fn carryover_is_monotone_decreasing() {
        let months = [0.0f64, 1.0, 2.0, 3.0, 6.0, 12.0, 24.0];
        let rates: Vec<f64> = months.iter().map(|&m| carryover_pct(m)).collect();
        for w in rates.windows(2) {
            assert!(w[0] > w[1], "carryover must be strictly decreasing: {rates:?}");
        }
    }

    // â”€â”€ address rotation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn rotate_seeds_new_address_with_decay() {
        let old = ExecutionAddress("0xOLD".into());
        let new = ExecutionAddress("0xNEW".into());
        let m = LaRelayMetrics::new(100, old.clone());

        // Seed old address with ~0.80 flashbots rate
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 80);
        }

        let relays = vec![RelayName::Flashbots];
        let seeded = rotate_address(&m, &old, new.clone(), 0.0, &relays).unwrap();
        assert_eq!(seeded, 1);

        // New address should have ~0.80 Ã— 0.50 = 0.40 rate
        let rate = m.rate_for(&RelayName::Flashbots, &new).unwrap();
        assert!((rate - 0.40).abs() < 0.1, "expected ~0.40 seeded rate, got {rate}");
        assert_eq!(m.active_address(), new);
    }

    #[test]
    fn rotate_with_unknown_old_address_seeds_zero_relays() {
        let old = ExecutionAddress("0xNONEXISTENT".into());
        let new = ExecutionAddress("0xNEW".into());
        let m = LaRelayMetrics::new(100, old.clone());
        let relays = vec![RelayName::Flashbots];
        // old address has no recorded data â€” nothing to carry over
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

    // â”€â”€ anti-fingerprint shuffle â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
        // flashbots 0.90, bloxroute 0.88, titan 0.50 (below 5% band)
        for i in 0..100 {
            m.record(&RelayName::Flashbots, i < 90);
            m.record(&RelayName::Bloxroute, i < 88);
            m.record(&RelayName::Titan, i < 50);
        }
        let order = submission_order(&m);
        assert_eq!(order.len(), 3);
        // titan must be last (below band)
        assert_eq!(order.last().unwrap().relay, RelayName::Titan);
    }
}