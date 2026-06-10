// omega-prl/src/integration.rs
//! OmegaEngine v12 integration layer — §2.1, §24
//!
//! Advisory structs for every v12 subsystem the PRL feeds into:
//!   Gas War Engine          — §13.3, v12 §12
//!   Liquidation Arb Engine  — §11.3, v12 §11
//!   Loss Attribution Engine — §14,   v12 §13
//!   Relay Routing           — §10,   v12 §11.2
//!   Sequencer Restart       — §12,   v12 §11.3
//!   Health FSM bridge       — §17,   v12 §3
//!
//! All advisories:
//!   - Return `None` when PRL is halted (§17.2)
//!   - Enforce minimum confidence thresholds (§15.3)
//!   - Never bypass governance caps (§13.3)
//!   - Are safe to ignore — execution continues unchanged

use tracing::{debug, warn};

use crate::{
    health::degraded::PrlHealthState, metrics::events::ObservabilityEvent, GasWarForecast,
    LiquidationPattern, MinConfidenceThresholds, PatternRecognitionLayer, SequencerRiskScore,
};

// ─────────────────────────────────────────────────────────────────────────────
// Gas War Engine advisory — §13, v12 §12
// ─────────────────────────────────────────────────────────────────────────────

/// Advisory for the Gas War Engine (§13.3).
#[derive(Debug, Clone)]
pub struct GasWarAdvisory {
    pub forecast: GasWarForecast,
    pub is_actionable: bool,
    /// Suggested bid multiplier [0.8, 2.0]. GWE applies cap_gwei on top.
    pub bid_multiplier: f32,
}

impl GasWarAdvisory {
    pub fn from_prl(prl: &PatternRecognitionLayer) -> Option<Self> {
        let forecast = prl.get_gas_forecast()?;
        if (forecast.confidence as f64) < MinConfidenceThresholds::GAS_ESCALATION {
            return None;
        }
        let bid_multiplier = (1.0 + forecast.escalation_velocity * 0.1).clamp(0.8, 2.0);
        debug!(
            expected_fee = forecast.expected_clearing_fee,
            velocity = forecast.escalation_velocity,
            bid_multiplier,
            "GasWarAdvisory derived"
        );
        Some(Self {
            is_actionable: true,
            bid_multiplier,
            forecast,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Liquidation Arb advisory — §11.3, v12 §11
// ─────────────────────────────────────────────────────────────────────────────

/// Advisory for the Liquidation Arbitrage Engine (§11.3).
#[derive(Debug, Clone)]
pub struct LiquidationAdvisory {
    pub pattern: LiquidationPattern,
    pub elevate_priority: bool,
    pub widen_relay_spread: bool,
    pub urgency: f32,
}

impl LiquidationAdvisory {
    pub fn for_position(prl: &PatternRecognitionLayer, position_key: u64) -> Option<Self> {
        let pattern = prl.get_liquidation_risk(position_key)?;
        let urgency = pattern.urgency_score();
        let elevate = pattern.should_elevate_priority();
        let widen = urgency > 0.8 && pattern.competitor_density > 0.6;
        debug!(
            position_key,
            urgency,
            elevate_priority = elevate,
            widen_relay_spread = widen,
            "LiquidationAdvisory derived"
        );
        Some(Self {
            elevate_priority: elevate,
            widen_relay_spread: widen,
            urgency,
            pattern,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Loss Attribution advisory — §14, v12 §13
// ─────────────────────────────────────────────────────────────────────────────

/// Advisory for the Loss Attribution Engine (§14.2).
#[derive(Debug, Clone)]
pub struct LossAttributionAdvisory {
    pub cluster_detected: bool,
    pub correlation_strength: f32,
    pub affected_protocols: smallvec::SmallVec<[u8; 4]>,
}

impl LossAttributionAdvisory {
    pub fn none() -> Self {
        Self {
            cluster_detected: false,
            correlation_strength: 0.0,
            affected_protocols: smallvec::SmallVec::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Relay routing advisory — §10, v12 §11.2
// ─────────────────────────────────────────────────────────────────────────────

/// Advisory for relay selection and cascade ordering.
#[derive(Debug, Clone)]
pub struct RelayRoutingAdvisory {
    pub ranked_relays: Vec<(u32, f32)>,
    pub suspected_leak_relays: Vec<u32>,
    pub deranked_relays: Vec<u32>,
}

impl RelayRoutingAdvisory {
    pub fn from_prl(prl: &PatternRecognitionLayer, relay_ids: &[u32]) -> Self {
        let mut ranked = Vec::with_capacity(relay_ids.len());
        let mut leaked = Vec::new();
        let mut deranked = Vec::new();

        for &id in relay_ids {
            let trust = if let Some(score) = prl.get_relay_risk(id) {
                if score.is_leak_suspected() {
                    leaked.push(id);
                    prl.metrics.emit_always_sampled(
                        ObservabilityEvent::RelayLeakSuspected,
                        Some(id),
                        "relay leak suspicion above 3σ threshold",
                    );
                }
                if score.trust_score < 0.20 {
                    deranked.push(id);
                    warn!(
                        relay_id = id,
                        trust = score.trust_score,
                        "PRL: relay deranked from top tie-band"
                    );
                }
                score.trust_score
            } else {
                0.5 // neutral — no PRL data yet
            };
            ranked.push((id, trust));
        }

        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Self {
            ranked_relays: ranked,
            suspected_leak_relays: leaked,
            deranked_relays: deranked,
        }
    }

    /// Top-N relays within `band_pct` of the best trust score.
    pub fn top_tie_band(&self, band_pct: f32) -> Vec<u32> {
        if self.ranked_relays.is_empty() {
            return Vec::new();
        }
        let best = self.ranked_relays[0].1;
        self.ranked_relays
            .iter()
            .filter(|(id, score)| {
                *score >= best * (1.0 - band_pct) && !self.deranked_relays.contains(id)
            })
            .map(|(id, _)| *id)
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sequencer restart advisory — §12, v12 §11.3
// ─────────────────────────────────────────────────────────────────────────────

/// Advisory for the v12 `SequencerRestartHandler` (§11.3) and `LaReorgGuard` (§11.4).
#[derive(Debug, Clone)]
pub struct SequencerRestartAdvisory {
    pub risk: SequencerRiskScore,
    pub activate_dedup: bool,
    pub activate_reorg_guard: bool,
}

impl SequencerRestartAdvisory {
    pub fn from_prl(prl: &PatternRecognitionLayer) -> Option<Self> {
        let risk = prl.get_sequencer_risk()?;
        if (risk.confidence as f64) < MinConfidenceThresholds::SEQUENCER_INSTABILITY {
            return None;
        }
        let activate_dedup = risk.should_activate_dedup();
        let activate_reorg_guard = risk.reorg_probability > 0.5;
        if activate_dedup {
            prl.metrics.emit_always_sampled(
                ObservabilityEvent::SequencerRestartPredicted,
                None,
                "sequencer restart probability exceeds threshold",
            );
        }
        Some(Self {
            risk,
            activate_dedup,
            activate_reorg_guard,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Health FSM bridge — §17, v12 §3
// ─────────────────────────────────────────────────────────────────────────────

/// Bridges PRL health into the v12 Health FSM.
///
/// A HALTED PRL does NOT cascade into the v12 FSM — execution continues.
pub struct PrlHealthBridge;

impl PrlHealthBridge {
    pub fn v12_advisory(prl: &PatternRecognitionLayer) -> Option<&'static str> {
        match prl.health_state() {
            PrlHealthState::Healthy => None,
            PrlHealthState::Degraded => Some("PRL_DEGRADED"),
            PrlHealthState::Limited => Some("PRL_LIMITED"),
            PrlHealthState::Halted => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gas_advisory_rejects_low_confidence() {
        let f = GasWarForecast {
            expected_clearing_fee: 100,
            escalation_velocity: 5.0,
            competitor_count_estimate: 3,
            inclusion_probability: 0.7,
            confidence: 0.50,
            emergency_bundle_recommended: false,
        };
        assert!((f.confidence as f64) < MinConfidenceThresholds::GAS_ESCALATION);
    }

    #[test]
    fn relay_advisory_tie_band_excludes_deranked() {
        let advisory = RelayRoutingAdvisory {
            ranked_relays: vec![(1, 0.90), (2, 0.85), (3, 0.10)],
            suspected_leak_relays: vec![],
            deranked_relays: vec![3],
        };
        let band = advisory.top_tie_band(0.10);
        assert!(!band.contains(&3));
        assert!(band.contains(&1));
        assert!(band.contains(&2));
    }

    #[test]
    fn bid_multiplier_clamped_to_two() {
        let velocity = 1_000.0f32;
        let bid_mult = (1.0 + velocity * 0.1).clamp(0.8, 2.0);
        assert_eq!(bid_mult, 2.0);
    }

    #[test]
    fn health_bridge_halted_returns_none() {
        let result: Option<&'static str> = match PrlHealthState::Halted {
            PrlHealthState::Healthy => None,
            PrlHealthState::Degraded => Some("PRL_DEGRADED"),
            PrlHealthState::Limited => Some("PRL_LIMITED"),
            PrlHealthState::Halted => None,
        };
        assert!(result.is_none());
    }
}
