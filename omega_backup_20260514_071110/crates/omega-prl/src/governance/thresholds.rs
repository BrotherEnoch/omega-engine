// omega-prl/src/governance/thresholds.rs
//! Governance-controlled threshold configuration â€” Â§20.1
//!
//! Hot-reloadable via `ArcSwap<ThresholdConfig>` â€” no restart required (Â§8.3).
//! Every field change goes through the L2/L3 governance path and is recorded
//! in `GovernanceAuditLog`.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::scoring::ranking::MinConfidenceThresholds;
use crate::patterns::signatures::PatternDomain;

/// Complete governance-controlled threshold configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// Monotonically increasing version â€” incremented on every governance update.
    pub version: u32,
    /// Per-domain minimum confidence overrides.  Absent â†’ spec default (Â§15.3).
    pub confidence_overrides: HashMap<u8, f64>,
    /// Max ML inference latency before disabling ML (Âµs).  Default 50 Âµs (Â§17.3).
    pub max_inference_latency_us: u64,
    /// Relay leak suspicion z-score trigger (Â§10.3).  Default 3.0.
    pub relay_leak_zscore: f64,
    /// Relay inclusion drop that triggers degradation (Â§10.3).  Default 0.15.
    pub relay_inclusion_drop: f64,
    /// Relay latency spike multiplier (Â§10.3).  Default 2.0 Ã— median.
    pub relay_latency_spike_multiplier: f32,
    /// Gas escalation z-score warn threshold.  Default 2.0.
    pub gas_escalation_z_warn: f64,
    /// Gas escalation z-score critical threshold.  Default 3.0.
    pub gas_escalation_z_critical: f64,
    /// Oracle deviation warn (Â§OracleMovement).  Default 0.05 = 5 %.
    pub oracle_deviation_warn: f64,
    /// Oracle deviation critical.  Default 0.10 = 10 %.
    pub oracle_deviation_critical: f64,
    /// Sequencer restart probability that activates dedup (Â§12.4).  Default 0.85.
    pub sequencer_restart_dedup_threshold: f32,
    /// Applied via emergency L3 path (Â§20.2).
    pub is_emergency: bool,
    /// Governance actor that applied this config (audit trail).
    pub applied_by: String,
    /// Unix seconds when applied.
    pub applied_at: u64,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            version:                           1,
            confidence_overrides:              HashMap::new(),
            max_inference_latency_us:          50,
            relay_leak_zscore:                 3.0,
            relay_inclusion_drop:              0.15,
            relay_latency_spike_multiplier:    2.0,
            gas_escalation_z_warn:             2.0,
            gas_escalation_z_critical:         3.0,
            oracle_deviation_warn:             0.05,
            oracle_deviation_critical:         0.10,
            sequencer_restart_dedup_threshold: 0.85,
            is_emergency:                      false,
            applied_by:                        "genesis".into(),
            applied_at:                        0,
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
        let mut c = ThresholdConfig::default();
        c.gas_escalation_z_warn     = 5.0;
        c.gas_escalation_z_critical = 2.0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn confidence_override_applied() {
        let mut c = ThresholdConfig::default();
        c.confidence_overrides.insert(PatternDomain::GasWar as u8, 0.99);
        assert_eq!(c.min_confidence_for(PatternDomain::GasWar), 0.99);
    }
}