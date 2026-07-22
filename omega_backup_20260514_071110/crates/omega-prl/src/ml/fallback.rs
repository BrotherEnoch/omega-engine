// omega-prl/src/ml/fallback.rs
//! Deterministic heuristic fallback â€” Â§16.3
//!
//! Used whenever the ML path is disabled (Â§17.2, Â§17.3).
//! Every heuristic is a simple, auditable weighted combination of feature
//! vector values â€” no trained weights, no opacity (Â§16.1).

use crate::features::extractor::{
    FeatureVector,
    F_LEAK_SUSPICION, F_RELAY_ACCEPT_BIAS, F_RELAY_DELAY_US,
    F_GAS_ESCALATION_VEL, F_BUNDLE_COMPLEXITY,
    F_HF_VELOCITY, F_ORACLE_MANIPULATION,
    F_SEARCHER_AGGRESSION, F_BUNDLE_RETRY_COUNT,
};
use crate::ml::inference::{
    InferenceResult, MODEL_RELAY, MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_SEARCHER,
};

/// Deterministic heuristic fallback (Â§16.3, Â§17.2).
pub struct DeterministicFallback;

impl DeterministicFallback {
    pub fn new() -> Self { Self }

    /// Dispatch to the per-model heuristic.  Always O(1), allocation-free.
    #[inline]
    pub fn infer(&self, model_name: &str, fv: &FeatureVector) -> InferenceResult {
        let probability = match model_name {
            MODEL_RELAY       => self.relay(fv),
            MODEL_GAS_WAR     => self.gas_war(fv),
            MODEL_LIQUIDATION => self.liquidation(fv),
            MODEL_SEARCHER    => self.searcher(fv),
            _                 => 0.5,
        };
        InferenceResult {
            probability,
            class_index: (probability > 0.5) as u8,
            latency_us:  1,
            from_ml:     false,
        }
    }

    // â”€â”€ Heuristics â€” auditable, no trained weights â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Relay risk: leak suspicion + low acceptance + high latency.
    #[inline]
    fn relay(&self, fv: &FeatureVector) -> f32 {
        let leak   = fv.values[F_LEAK_SUSPICION];
        let bias   = fv.values[F_RELAY_ACCEPT_BIAS];
        let lat    = fv.values[F_RELAY_DELAY_US];
        (0.5 * leak + 0.3 * (1.0 - bias.max(0.0)) + 0.2 * lat).clamp(0.0, 1.0)
    }

    /// Gas war intensity: escalation velocity + bundle complexity.
    #[inline]
    fn gas_war(&self, fv: &FeatureVector) -> f32 {
        let vel = fv.values[F_GAS_ESCALATION_VEL];
        let cmp = fv.values[F_BUNDLE_COMPLEXITY];
        (vel.abs() * 0.7 + cmp * 0.3).clamp(0.0, 1.0)
    }

    /// Liquidation urgency: HF velocity + oracle manipulation signal.
    #[inline]
    fn liquidation(&self, fv: &FeatureVector) -> f32 {
        let hf = fv.values[F_HF_VELOCITY];
        let om = fv.values[F_ORACLE_MANIPULATION];
        (hf.abs() * 0.6 + om * 0.4).clamp(0.0, 1.0)
    }

    /// Searcher aggression: aggression score + retry cadence.
    #[inline]
    fn searcher(&self, fv: &FeatureVector) -> f32 {
        let ag = fv.values[F_SEARCHER_AGGRESSION];
        let rt = fv.values[F_BUNDLE_RETRY_COUNT];
        (ag * 0.7 + rt * 0.3).clamp(0.0, 1.0)
    }
}

impl Default for DeterministicFallback {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_models_return_valid_probability() {
        let fb = DeterministicFallback::new();
        let fv = FeatureVector::zeroed();
        for name in [MODEL_RELAY, MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_SEARCHER] {
            let r = fb.infer(name, &fv);
            assert!(r.probability >= 0.0 && r.probability <= 1.0,
                "{name}: probability out of range: {}", r.probability);
            assert!(!r.from_ml);
            assert_eq!(r.latency_us, 1);
        }
    }

    #[test]
    fn unknown_model_returns_half() {
        let fb = DeterministicFallback::new();
        let fv = FeatureVector::zeroed();
        let r  = fb.infer("unknown-model", &fv);
        assert!((r.probability - 0.5).abs() < f32::EPSILON);
    }
}