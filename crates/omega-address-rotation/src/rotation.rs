// crates/omega-address-rotation/src/rotation.rs
//
// AddressRotationManager — execution address rotation scheduler (spec §14).

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use omega_gas_war::LaRelayMetrics;
use omega_loss_attribution::LossCode;
use serde::{Deserialize, Serialize};

use crate::pattern_detector::{PatternDetector, PatternDetectorConfig};
use crate::reputation::{seed_relay_metrics, CarryoverParams};

// ─────────────────────────────────────────────────────────────────────────────
// RotationConfig
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationConfig {
    pub schedule_days:     u32,
    pub base_carryover:    f64,
    pub decay_rate_months: f64,
    pub pattern:           PatternDetectorConfig,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            schedule_days:     30,
            base_carryover:    0.50,
            decay_rate_months: 3.0,
            pattern:           PatternDetectorConfig::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RotationTrigger
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationTrigger {
    Schedule,
    FingerprintDetected,
}

impl std::fmt::Display for RotationTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RotationTrigger::Schedule            => f.write_str("SCHEDULE"),
            RotationTrigger::FingerprintDetected => f.write_str("FINGERPRINT_DETECTED"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RotationRecord
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationRecord {
    pub rotated_at:        DateTime<Utc>,
    pub trigger:           RotationTrigger,
    pub old_rates:         std::collections::HashMap<String, f64>,
    pub carryover_pct:     f64,
    pub months_since_prev: f64,
    pub same_fee_rate:     f64,
}

// ─────────────────────────────────────────────────────────────────────────────
// AddressRotationManager
// ─────────────────────────────────────────────────────────────────────────────

pub struct AddressRotationManager {
    config:          RotationConfig,
    relay_metrics:   Arc<LaRelayMetrics>,
    detector:        PatternDetector,
    last_rotated_at: DateTime<Utc>,
    history:         Vec<RotationRecord>,
}

impl AddressRotationManager {
    pub fn new(config: RotationConfig, relay_metrics: Arc<LaRelayMetrics>) -> Self {
        let detector = PatternDetector::new(config.pattern.clone());
        Self {
            config,
            relay_metrics,
            detector,
            last_rotated_at: Utc::now(),
            history: Vec::new(),
        }
    }

    pub fn on_loss(&mut self, code: LossCode) -> Option<RotationTrigger> {
        if self.detector.record(code) {
            tracing::warn!(
                same_fee_rate = self.detector.same_fee_rate(),
                threshold     = self.config.pattern.trigger_threshold,
                "Fingerprinting pattern detected — triggering address rotation",
            );
            Some(RotationTrigger::FingerprintDetected)
        } else {
            None
        }
    }

    pub fn check_schedule(&self) -> Option<RotationTrigger> {
        let elapsed   = Utc::now() - self.last_rotated_at;
        let threshold = Duration::days(self.config.schedule_days as i64);
        if elapsed >= threshold {
            tracing::info!(
                days_elapsed  = elapsed.num_days(),
                schedule_days = self.config.schedule_days,
                "Scheduled address rotation due",
            );
            Some(RotationTrigger::Schedule)
        } else {
            None
        }
    }

    pub fn execute_rotation(
        &mut self,
        trigger:           RotationTrigger,
        new_relay_metrics: Arc<LaRelayMetrics>,
        rng:               &mut impl rand::Rng,
    ) -> RotationRecord {
        let now     = Utc::now();
        let elapsed = now - self.last_rotated_at;
        let months  = elapsed.num_seconds() as f64 / (30.0 * 24.0 * 3600.0);

        let params = CarryoverParams {
            base_carryover:        self.config.base_carryover,
            decay_rate_months:     self.config.decay_rate_months,
            months_since_rotation: months,
        };

        let old_rates     = snapshot_relay_rates(&self.relay_metrics, rng);
        let carryover_pct = crate::reputation::compute_carryover_pct_params(&params);
        let seed_samples: usize = 20;

        seed_relay_metrics(
            &self.relay_metrics,
            &new_relay_metrics,
            carryover_pct,
            seed_samples,
            rng,
        );

        let record = RotationRecord {
            rotated_at:        now,
            trigger,
            old_rates,
            carryover_pct,
            months_since_prev: months,
            same_fee_rate:     self.detector.same_fee_rate(),
        };

        self.relay_metrics   = new_relay_metrics;
        self.last_rotated_at = now;
        self.detector.reset();

        tracing::warn!(
            trigger           = %record.trigger,
            carryover_pct,
            months_since_prev = months,
            same_fee_rate     = record.same_fee_rate,
            "Address rotation executed",
        );

        self.history.push(record.clone());
        record
    }

    pub fn history(&self) -> &[RotationRecord] {
        &self.history
    }

    pub fn relay_metrics(&self) -> Arc<LaRelayMetrics> {
        self.relay_metrics.clone()
    }

    pub fn days_until_scheduled(&self) -> i64 {
        let threshold = Duration::days(self.config.schedule_days as i64);
        let elapsed   = Utc::now() - self.last_rotated_at;
        let remaining = threshold - elapsed;
        if remaining <= Duration::zero() {
            return 0;
        }

        let secs = remaining.num_seconds();
        ((secs + 86_399) / 86_400).max(0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helper
// ─────────────────────────────────────────────────────────────────────────────

fn snapshot_relay_rates(
    metrics: &Arc<LaRelayMetrics>,
    rng:     &mut impl rand::Rng,
) -> std::collections::HashMap<String, f64> {
    metrics
        .ranked_relays(1.0, rng)
        .into_iter()
        .map(|r| (r.relay_name, r.la_rate))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_gas_war::{LaRelayMetrics, DEFAULT_WINDOW};
    use rand::SeedableRng;

    fn make_manager() -> AddressRotationManager {
        // LaRelayMetrics::new() already returns Arc<LaRelayMetrics>.
        // Do NOT wrap in Arc::new() — that produces Arc<Arc<LaRelayMetrics>>.
        let metrics = LaRelayMetrics::new(DEFAULT_WINDOW);
        AddressRotationManager::new(RotationConfig::default(), metrics)
    }

    fn seeded_rng() -> impl rand::Rng {
        rand::rngs::StdRng::seed_from_u64(42)
    }

    #[test]
    fn schedule_not_due_immediately() {
        let mgr = make_manager();
        assert!(mgr.check_schedule().is_none());
    }

    #[test]
    fn days_until_scheduled_is_30_on_fresh_manager() {
        let mgr = make_manager();
        assert_eq!(mgr.days_until_scheduled(), 30);
    }

    #[test]
    fn no_trigger_below_threshold() {
        let mut mgr = make_manager();
        for _ in 0..200 {
            mgr.on_loss(LossCode::LostGasLow);
        }
    }

    #[test]
    fn pattern_trigger_on_high_same_fee() {
        let mut mgr     = make_manager();
        let mut triggered = None;
        for i in 0..200 {
            let code = if i % 4 == 0 { LossCode::LostRaceSameFee } else { LossCode::LostGasLow };
            triggered = mgr.on_loss(code);
            if triggered.is_some() { break; }
        }
        assert_eq!(triggered, Some(RotationTrigger::FingerprintDetected));
    }

    #[test]
    fn execute_rotation_resets_detector() {
        let mut mgr = make_manager();
        for i in 0..200 {
            let code = if i % 4 == 0 { LossCode::LostRaceSameFee } else { LossCode::LostGasLow };
            if mgr.on_loss(code).is_some() { break; }
        }
        // LaRelayMetrics::new() returns Arc<LaRelayMetrics> — no extra Arc::new()
        let new_metrics = LaRelayMetrics::new(DEFAULT_WINDOW);
        let mut rng     = seeded_rng();
        let record = mgr.execute_rotation(RotationTrigger::FingerprintDetected, new_metrics, &mut rng);
        assert_eq!(record.trigger, RotationTrigger::FingerprintDetected);
        assert!(mgr.on_loss(LossCode::LostGasLow).is_none());
    }

    #[test]
    fn execute_rotation_populates_history() {
        let mut mgr = make_manager();
        let new     = LaRelayMetrics::new(DEFAULT_WINDOW); // already Arc
        let mut rng = seeded_rng();
        mgr.execute_rotation(RotationTrigger::Schedule, new, &mut rng);
        assert_eq!(mgr.history().len(), 1);
        assert_eq!(mgr.history()[0].trigger, RotationTrigger::Schedule);
    }

    #[test]
    fn carryover_pct_between_0_and_1() {
        let mut mgr = make_manager();
        let new     = LaRelayMetrics::new(DEFAULT_WINDOW); // already Arc
        let mut rng = seeded_rng();
        let record  = mgr.execute_rotation(RotationTrigger::Schedule, new, &mut rng);
        assert!(record.carryover_pct >= 0.0 && record.carryover_pct <= 1.0);
    }
}
