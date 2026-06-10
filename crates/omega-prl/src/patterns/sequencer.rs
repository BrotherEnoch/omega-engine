// omega-prl/src/patterns/sequencer.rs
//! Sequencer & Reorg Pattern Detection — §12

/// §12.3 — Sequencer and reorg risk assessment.
#[derive(Debug, Clone)]
pub struct SequencerRiskScore {
    pub instability_score: f32,
    pub restart_probability: f32,
    pub reorg_probability: f32,
    pub confidence: f32,
}

impl SequencerRiskScore {
    /// Compute from observed block timing signals.
    ///
    /// - `jitter_ratio`     — std_dev / mean of block intervals
    /// - `hash_instability` — fraction of recent blocks with changed hashes
    /// - `rpc_divergence`   — fraction of RPC responses with divergent state
    pub fn from_observed(jitter_ratio: f32, hash_instability: f32, rpc_divergence: f32) -> Self {
        let instability =
            (0.5 * jitter_ratio + 0.3 * hash_instability + 0.2 * rpc_divergence).clamp(0.0, 1.0);
        let restart_prob = (instability * 1.2 - 0.2).clamp(0.0, 1.0);
        let reorg_prob = (hash_instability * 1.5).clamp(0.0, 1.0);
        let confidence = 1.0 - rpc_divergence.clamp(0.0, 0.5);
        Self {
            instability_score: instability,
            restart_probability: restart_prob,
            reorg_probability: reorg_prob,
            confidence,
        }
    }

    /// §12.4 — Whether to pre-emptively activate the 60-block dedup window.
    /// Integrates with `SequencerRestartHandler` (v12 §11.3).
    #[inline]
    pub fn should_activate_dedup(&self) -> bool {
        self.restart_probability > 0.85 && self.confidence > 0.70
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_observed_in_bounds() {
        let r = SequencerRiskScore::from_observed(1.5, 0.6, 0.1);
        assert!(r.restart_probability >= 0.0 && r.restart_probability <= 1.0);
        assert!(r.reorg_probability >= 0.0 && r.reorg_probability <= 1.0);
        assert!(r.confidence >= 0.0 && r.confidence <= 1.0);
    }

    #[test]
    fn high_instability_activates_dedup() {
        let r = SequencerRiskScore::from_observed(2.0, 0.8, 0.05);
        assert!(
            r.should_activate_dedup(),
            "restart_prob={}, confidence={}",
            r.restart_probability,
            r.confidence
        );
    }
}
