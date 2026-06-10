// crates/omega-observability/src/sampler.rs
//
// Event sampler — 100% sampling for LA events, configurable for others (§16).
//
// ## Spec §16
//
//   LA events are always-sampled (100% sampling rate).  See
//   `OmegaEvent::is_always_sampled()` for the canonical list.
//
//   All other events use the configured `sample_rate` (0.0–1.0).
//   A rate of 1.0 means 100% (no sampling); 0.0 means reject all.
//
// ## Implementation
//
//   The sampler is deterministic for LA events (always pass) and uses
//   the event's timestamp nanoseconds as lightweight entropy for non-LA
//   events.  This avoids a mutex on a shared RNG while producing an
//   approximately correct sample rate over many events.
//
//   `Sampler` is `Clone` and `Send + Sync` — multiple exporter tasks
//   can share a sampler via `Arc<Sampler>` without contention.

use crate::events::OmegaEvent;

// ─────────────────────────────────────────────────────────────────────────────
// Sampler
// ─────────────────────────────────────────────────────────────────────────────

/// Event sampler enforcing §16 sampling policy.
///
/// LA events always pass (`should_emit` returns `true`).
/// Non-LA events pass with probability `sample_rate`.
#[derive(Debug, Clone)]
pub struct Sampler {
    /// Sampling rate for non-LA events.  Range: [0.0, 1.0].
    /// 1.0 = 100% (no sampling); 0.0 = reject all non-LA events.
    sample_rate: f64,
}

impl Sampler {
    /// Create a sampler with the given rate for non-LA events.
    ///
    /// `sample_rate` is clamped to [0.0, 1.0].
    pub fn new(sample_rate: f64) -> Self {
        Self {
            sample_rate: sample_rate.clamp(0.0, 1.0),
        }
    }

    /// Production default: 100% sampling for all events.
    pub fn full() -> Self {
        Self::new(1.0)
    }

    /// Returns `true` when the event should be exported.
    ///
    /// LA events (§16 always-sampled) always return `true`.
    /// Non-LA events return `true` with probability `sample_rate`.
    pub fn should_emit(&self, event: &OmegaEvent) -> bool {
        if event.is_always_sampled() {
            return true;
        }

        // Fast path: full sampling.
        if self.sample_rate >= 1.0 {
            return true;
        }
        // Fast path: no sampling.
        if self.sample_rate <= 0.0 {
            return false;
        }

        // Deterministic per-event decision using the event's timestamp
        // nanoseconds as entropy.  `DateTime::timestamp_nanos_opt()` returns
        // `Option<i64>`; we use 0 as fallback (safe — only affects bucket
        // placement, not correctness of the always-sampled path above).
        let nanos = event
            .timestamp()
            .timestamp_nanos_opt()
            .unwrap_or(0)
            .unsigned_abs();
        let bucket = (nanos % 1_000_000) as f64 / 1_000_000.0;
        bucket < self.sample_rate
    }

    /// Current sample rate for non-LA events.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::OmegaEvent;
    use chrono::Utc;

    /// An always-sampled LA event.
    fn la_event() -> OmegaEvent {
        OmegaEvent::BlueprintDropped {
            blueprint_hash: "aabbcc".into(),
            strategy_id: "LA".into(),
            drop_code: "MISS_HF_NOT_LIQUIDATABLE".into(),
            chain_id: 42161,
            timestamp: Utc::now(),
        }
    }

    /// A non-always-sampled event.
    fn non_la_event() -> OmegaEvent {
        OmegaEvent::OraclePriceResolved {
            timestamp: Utc::now(),
            asset: "ETH".into(),
            price_usd: 3000.0,
            source: "chainlink".into(),
            age_seconds: 1,
            chain_id: 42161,
        }
    }

    // ── Always-sampled LA events ──────────────────────────────────────────

    #[test]
    fn la_events_always_pass_at_zero_rate() {
        let sampler = Sampler::new(0.0);
        assert!(
            sampler.should_emit(&la_event()),
            "LA events must be always-sampled even at rate 0.0 (§16)"
        );
    }

    #[test]
    fn la_events_always_pass_at_full_rate() {
        let sampler = Sampler::full();
        assert!(sampler.should_emit(&la_event()));
    }

    // ── Non-LA events ─────────────────────────────────────────────────────

    #[test]
    fn non_la_at_zero_rate_always_rejected() {
        let sampler = Sampler::new(0.0);
        let mut any_passed = false;
        for _ in 0..1_000 {
            if sampler.should_emit(&non_la_event()) {
                any_passed = true;
                break;
            }
        }
        assert!(
            !any_passed,
            "non-LA events at rate 0.0 must always be rejected"
        );
    }

    #[test]
    fn non_la_at_full_rate_always_passes() {
        let sampler = Sampler::full();
        assert!(sampler.should_emit(&non_la_event()));
    }

    // ── Approximate rate over many events ─────────────────────────────────

    #[test]
    fn sample_rate_50pct_approximately_correct() {
        let sampler = Sampler::new(0.5);
        let total = 10_000u64;
        // Use OracleDiverge with varying diverge_bps so each event has a
        // different timestamp_nanos bucket (Utc::now() ticks fast enough
        // over 10k iterations in practice; diverge_bps variation is extra
        // insurance that the events differ).
        let passed = (0..total)
            .filter(|&i| {
                let ev = OmegaEvent::OracleDiverge {
                    timestamp: Utc::now(),
                    asset: "ETH".into(),
                    price_primary: 3000.0,
                    price_secondary: 3001.0,
                    diverge_bps: i,
                    chain_id: 42161,
                };
                sampler.should_emit(&ev)
            })
            .count();
        let rate = passed as f64 / total as f64;
        assert!(
            rate > 0.40 && rate < 0.60,
            "expected ~50% pass rate at 0.5, got {rate:.3}",
        );
    }

    // ── Sample rate clamping ──────────────────────────────────────────────

    #[test]
    fn sample_rate_clamped_above_1() {
        let sampler = Sampler::new(2.0);
        assert!((sampler.sample_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sample_rate_clamped_below_0() {
        let sampler = Sampler::new(-0.5);
        assert!((sampler.sample_rate() - 0.0).abs() < f64::EPSILON);
    }

    // ── Always-sampled variants ───────────────────────────────────────────

    #[test]
    fn emergency_halt_is_always_sampled() {
        let sampler = Sampler::new(0.0);
        let event = OmegaEvent::EmergencyHalt {
            timestamp: Utc::now(),
            issuer: "governance".into(),
            reason: "test".into(),
        };
        assert!(
            sampler.should_emit(&event),
            "EmergencyHalt must be always-sampled (§16)"
        );
    }

    #[test]
    fn profit_split_is_always_sampled() {
        let sampler = Sampler::new(0.0);
        let event = OmegaEvent::ProfitSplit {
            timestamp: Utc::now(),
            blueprint_hash: "deadbeef".into(),
            pil_share_eth: 0.95,
            dao_fee_eth: 0.05,
            dao_fee_address: "0xDAO".into(),
            chain_id: 42161,
        };
        assert!(
            sampler.should_emit(&event),
            "ProfitSplit must be always-sampled (§16)"
        );
    }

    #[test]
    fn la_reorg_risk_is_always_sampled() {
        let sampler = Sampler::new(0.0);
        let event = OmegaEvent::LaReorgRisk {
            timestamp: Utc::now(),
            tx_hash: "cafebabe".into(),
            orphaned_block: 100,
            rescore_at: 105,
        };
        assert!(sampler.should_emit(&event));
    }

    #[test]
    fn gas_model_reverted_is_always_sampled() {
        let sampler = Sampler::new(0.0);
        let event = OmegaEvent::GasModelReverted {
            timestamp: Utc::now(),
            checkpoint_version: 3,
            checkpoint_rate: 0.72,
            holdout_rate: 0.65,
            degradation_pct: 9.7,
        };
        assert!(sampler.should_emit(&event));
    }
}
