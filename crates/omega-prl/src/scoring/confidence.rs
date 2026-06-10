// omega-prl/src/scoring/confidence.rs
//! Confidence score structure — §15.1
//!
//! Every emitted pattern MUST expose all five fields:
//!   score, confidence, evidence, temporal_stability, freshness (§15.1).

use crate::patterns::signatures::PatternId;
use crate::scoring::decay::decay_confidence;

/// §15.1 — Complete confidence record for one pattern match.
#[derive(Debug, Clone, Copy)]
pub struct ConfidenceScore {
    pub pattern_id: PatternId,
    /// Raw pattern score (domain-specific units).
    pub score: f64,
    /// Normalised confidence [0, 1] after time decay.
    pub confidence: f64,
    /// Number of evidence events contributing to this score.
    pub evidence: u32,
    /// Age of the score in nanoseconds (for freshness computation).
    pub age_ns: u64,
}

impl ConfidenceScore {
    /// Variance estimate approximated from evidence count.
    /// More evidence → lower variance.  Used by ranking (§15.3).
    #[inline]
    pub fn variance_estimate(&self) -> f64 {
        if self.evidence == 0 {
            return 1.0;
        }
        1.0 / (self.evidence as f64).sqrt()
    }

    /// Freshness score [0, 1] — 1.0 for brand-new, approaches 0 as score ages.
    #[inline]
    pub fn freshness(&self) -> f64 {
        let age_ms = self.age_ns / 1_000_000;
        decay_confidence(1.0, age_ms)
    }

    /// Whether this score meets a minimum confidence threshold.
    #[inline]
    pub fn is_actionable(&self, min_confidence: f64) -> bool {
        self.confidence >= min_confidence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variance_decreases_with_evidence() {
        let low = ConfidenceScore {
            pattern_id: PatternId(0),
            score: 1.0,
            confidence: 0.9,
            evidence: 5,
            age_ns: 0,
        };
        let high = ConfidenceScore {
            evidence: 1000,
            ..low
        };
        assert!(high.variance_estimate() < low.variance_estimate());
    }

    #[test]
    fn freshness_degrades_with_age() {
        let fresh = ConfidenceScore {
            pattern_id: PatternId(0),
            score: 1.0,
            confidence: 0.9,
            evidence: 1,
            age_ns: 0,
        };
        let stale = ConfidenceScore {
            age_ns: 10_000_000_000,
            ..fresh
        }; // 10s
        assert!(stale.freshness() < fresh.freshness());
    }

    #[test]
    fn is_actionable_threshold() {
        let s = ConfidenceScore {
            pattern_id: PatternId(0),
            score: 1.0,
            confidence: 0.85,
            evidence: 1,
            age_ns: 0,
        };
        assert!(s.is_actionable(0.80));
        assert!(!s.is_actionable(0.90));
    }
}
