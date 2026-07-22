// omega-prl/src/governance/thresholds.rs
//! Governance-controlled threshold configuration — §20.1
//!
//! ## Audit fix (this revision)
//!
//! `validate()` previously checked 3 of the ~13 governance-controlled
//! fields on this struct. `oracle_deviation_warn`/`_critical` had no
//! ordering check even though the identical bug class (warn ≥ critical)
//! was already caught for `gas_escalation_z_warn`/`_critical` two lines
//! away. `relay_leak_zscore`, `relay_latency_spike_multiplier`,
//! `sequencer_restart_dedup_threshold`, and `confidence_overrides`
//! values were entirely unvalidated — a governance action could set any
//! of these to a nonsensical value (a negative z-score, a sub-1.0
//! "spike" multiplier, an override outside [0,1]) with no rejection.
//! All are now validated.

use crate::patterns::signatures::PatternDomain;
use crate::scoring::ranking::MinConfidenceThresholds;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Complete governance-controlled threshold configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    pub version:                          u32,
    pub confidence_overrides:             HashMap<u8, f64>,
    pub max_inference_latency_us:         u64,
    pub relay_leak_zscore:                f64,
    pub relay_inclusion_drop:             f64,
    pub relay_latency_spike_multiplier:   f32,
    pub gas_escalation_z_warn:            f64,
    pub gas_escalation_z_critical:        f64,
    pub oracle_deviation_warn:            f64,
    pub oracle_deviation_critical:        f64,
    pub sequencer_restart_dedup_threshold: f32,
    pub is_emergency:                     bool,
    pub applied_by:                       String,
    pub applied_at:                       u64,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            version:                          1,
            confidence_overrides:             HashMap::new(),
            max_inference_latency_us:         50,
            relay_leak_zscore:                3.0,
            relay_inclusion_drop:             0.15,
            relay_latency_spike_multiplier:   2.0,
            gas_escalation_z_warn:            2.0,
            gas_escalation_z_critical:        3.0,
            oracle_deviation_warn:            0.05,
            oracle_deviation_critical:        0.10,
            sequencer_restart_dedup_threshold: 0.85,
            is_emergency:                     false,
            applied_by:                       "genesis".into(),
            applied_at:                       0,
        }
    }
}

impl ThresholdConfig {
    /// Minimum confidence for a domain, with any governance override applied.
    #[inline]
    pub fn min_confidence_for(&self, domain: PatternDomain) -> f64 {
        if let Some(&ov) = self.confidence_overrides.get(&(domain as u8)) {
            return ov;
        }
        MinConfidenceThresholds::for_domain(domain)
    }

    /// Validate all fields are within safe governance bounds.
    pub fn validate(&self) -> Result<(), String> {
        if self.relay_inclusion_drop <= 0.0 || self.relay_inclusion_drop > 0.5 {
            return Err(format!(
                "relay_inclusion_drop out of range: {}",
                self.relay_inclusion_drop
            ));
        }
        if self.max_inference_latency_us < 10 || self.max_inference_latency_us > 1_000 {
            return Err(format!(
                "max_inference_latency_us out of range: {}",
                self.max_inference_latency_us
            ));
        }
        if self.gas_escalation_z_warn >= self.gas_escalation_z_critical {
            return Err("gas_escalation_z_warn must be < z_critical".into());
        }
        // Previously unchecked — same bug class as the gas z-score pair
        // above, just missed for oracle deviation.
        if self.oracle_deviation_warn >= self.oracle_deviation_critical {
            return Err("oracle_deviation_warn must be < oracle_deviation_critical".into());
        }
        if self.relay_leak_zscore <= 0.0 {
            return Err(format!(
                "relay_leak_zscore must be > 0.0: {}",
                self.relay_leak_zscore
            ));
        }
        if self.relay_latency_spike_multiplier <= 1.0 {
            return Err(format!(
                "relay_latency_spike_multiplier must be > 1.0 (a sub-1.0 \
                 'spike' multiplier is nonsensical): {}",
                self.relay_latency_spike_multiplier
            ));
        }
        if !(0.0..=1.0).contains(&self.sequencer_restart_dedup_threshold) {
            return Err(format!(
                "sequencer_restart_dedup_threshold out of range [0.0, 1.0]: {}",
                self.sequencer_restart_dedup_threshold
            ));
        }
        for (&domain, &value) in &self.confidence_overrides {
            if !(0.0..=1.0).contains(&value) {
                return Err(format!(
                    "confidence_overrides[{domain}] out of range [0.0, 1.0]: {value}"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        assert!(ThresholdConfig::default().validate().is_ok());
    }

    #[test]
    fn bad_z_order_fails() {
        let c = ThresholdConfig {
            gas_escalation_z_warn:     5.0,
            gas_escalation_z_critical: 2.0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn confidence_override_applied() {
        let mut c = ThresholdConfig::default();
        c.confidence_overrides
            .insert(PatternDomain::GasWar as u8, 0.99);
        assert_eq!(c.min_confidence_for(PatternDomain::GasWar), 0.99);
    }

    #[test]
    fn oracle_deviation_bad_order_fails() {
        let c = ThresholdConfig {
            oracle_deviation_warn:     0.20,
            oracle_deviation_critical: 0.10,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn relay_leak_zscore_must_be_positive() {
        let c = ThresholdConfig {
            relay_leak_zscore: 0.0,
            ..Default::default()
        };
        assert!(c.validate().is_err());

        let c2 = ThresholdConfig {
            relay_leak_zscore: -1.0,
            ..Default::default()
        };
        assert!(c2.validate().is_err());
    }

    #[test]
    fn spike_multiplier_below_one_fails() {
        let c = ThresholdConfig {
            relay_latency_spike_multiplier: 0.5,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn spike_multiplier_exactly_one_fails() {
        // Exactly 1.0 means "no spike at all" is the trigger — nonsensical,
        // must be strictly greater than 1.0.
        let c = ThresholdConfig {
            relay_latency_spike_multiplier: 1.0,
            ..Default::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn dedup_threshold_out_of_range_fails() {
        let c = ThresholdConfig {
            sequencer_restart_dedup_threshold: 1.5,
            ..Default::default()
        };
        assert!(c.validate().is_err());

        let c2 = ThresholdConfig {
            sequencer_restart_dedup_threshold: -0.1,
            ..Default::default()
        };
        assert!(c2.validate().is_err());
    }

    #[test]
    fn confidence_override_out_of_range_fails() {
        let mut c = ThresholdConfig::default();
        c.confidence_overrides.insert(PatternDomain::GasWar as u8, 1.5);
        assert!(c.validate().is_err());

        let mut c2 = ThresholdConfig::default();
        c2.confidence_overrides.insert(PatternDomain::GasWar as u8, -0.2);
        assert!(c2.validate().is_err());
    }

    #[test]
    fn valid_confidence_override_still_passes() {
        let mut c = ThresholdConfig::default();
        c.confidence_overrides.insert(PatternDomain::GasWar as u8, 0.9);
        assert!(c.validate().is_ok());
    }
}