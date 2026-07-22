// omega-prl/src/scoring/ranking.rs
//! Confidence thresholds and ranked output â€” Â§15.3

use crate::patterns::signatures::PatternDomain;
use crate::scoring::confidence::ConfidenceScore;

/// Â§15.3 â€” Minimum confidence thresholds per signal type.
pub struct MinConfidenceThresholds;

impl MinConfidenceThresholds {
    pub const RELAY_BLACKLIST:       f64 = 0.95;
    pub const GAS_ESCALATION:        f64 = 0.80;
    pub const SEQUENCER_INSTABILITY: f64 = 0.85;
    pub const SEARCHER_FINGERPRINT:  f64 = 0.70;
    pub const ORACLE_ANOMALY:        f64 = 0.90;

    /// Threshold for a given pattern domain.
    #[inline]
    pub fn for_domain(domain: PatternDomain) -> f64 {
        match domain {
            PatternDomain::RelayBehavior       => Self::RELAY_BLACKLIST,
            PatternDomain::BuilderManipulation => Self::RELAY_BLACKLIST,
            PatternDomain::GasWar              => Self::GAS_ESCALATION,
            PatternDomain::ProfitabilityDrift  => Self::GAS_ESCALATION,
            PatternDomain::SequencerStability  => Self::SEQUENCER_INSTABILITY,
            PatternDomain::SearcherFingerprint => Self::SEARCHER_FINGERPRINT,
            PatternDomain::OracleMovement      => Self::ORACLE_ANOMALY,
            _                                  => 0.75,
        }
    }
}

/// Return scores above the domain threshold, sorted by confidence descending.
pub fn rank_scores(scores: &[ConfidenceScore], domain: PatternDomain) -> Vec<ConfidenceScore> {
    let min_conf = MinConfidenceThresholds::for_domain(domain);
    let mut out: Vec<ConfidenceScore> = scores.iter()
        .copied()
        .filter(|s| s.confidence >= min_conf)
        .collect();
    out.sort_by(|a, b| {
        b.confidence.partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::signatures::PatternId;

    fn s(id: u64, conf: f64) -> ConfidenceScore {
        ConfidenceScore { pattern_id: PatternId(id), score: 1.0,
            confidence: conf, evidence: 1, age_ns: 0 }
    }

    #[test]
    fn relay_threshold_is_095() {
        assert_eq!(MinConfidenceThresholds::for_domain(PatternDomain::RelayBehavior), 0.95);
    }

    #[test]
    fn rank_filters_and_sorts() {
        let scores = vec![s(1, 0.99), s(2, 0.50), s(3, 0.97)];
        let ranked = rank_scores(&scores, PatternDomain::RelayBehavior);
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].confidence >= ranked[1].confidence);
    }
}