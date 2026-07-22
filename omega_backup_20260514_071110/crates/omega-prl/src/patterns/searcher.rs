// omega-prl/src/patterns/searcher.rs
//! Searcher Fingerprinting Engine â€” Â§9

/// Â§9.3 â€” Searcher behaviour fingerprint.
#[derive(Debug, Clone)]
pub struct SearcherFingerprint {
    pub fingerprint_id:         u64,
    pub relay_affinity:         [u16; 16],
    pub escalation_curve:       [f32; 8],
    pub retry_pattern:          [u8; 16],
    pub inclusion_latency_mean: u32,
    pub confidence:             f32,
}

impl SearcherFingerprint {
    /// Weighted similarity to another fingerprint [0, 1].
    /// Used for competitor identification (Â§9.4).
    pub fn similarity(&self, other: &Self) -> f32 {
        // Relay affinity cosine similarity
        let relay_sim = {
            let dot: u64 = self.relay_affinity.iter()
                .zip(&other.relay_affinity)
                .map(|(&a, &b)| a as u64 * b as u64)
                .sum();
            let na: u64 = self.relay_affinity.iter().map(|&x| (x as u64).pow(2)).sum();
            let nb: u64 = other.relay_affinity.iter().map(|&x| (x as u64).pow(2)).sum();
            let denom = ((na as f64) * (nb as f64)).sqrt();
            if denom < 1.0 { 0.0f32 } else { (dot as f64 / denom) as f32 }
        };

        // Escalation curve L2 distance â†’ similarity
        let esc_dist: f32 = self.escalation_curve.iter()
            .zip(&other.escalation_curve)
            .map(|(&a, &b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();
        let esc_sim = (1.0 - esc_dist / 8.0_f32.sqrt()).clamp(0.0, 1.0);

        // Latency similarity
        let lat_diff = (self.inclusion_latency_mean as f32
            - other.inclusion_latency_mean as f32)
            .abs() / 10_000.0;
        let lat_sim = (1.0 - lat_diff).clamp(0.0, 1.0);

        (0.40 * relay_sim + 0.40 * esc_sim + 0.20 * lat_sim).clamp(0.0, 1.0)
    }

    /// Â§9.4 â€” Actionable when confidence â‰¥ 0.70 (Â§15.3).
    #[inline]
    pub fn is_actionable(&self) -> bool {
        self.confidence >= 0.70
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_fingerprint_similarity_is_one() {
        let fp = SearcherFingerprint {
            fingerprint_id:         1,
            relay_affinity:         [100u16; 16],
            escalation_curve:       [1.0f32; 8],
            retry_pattern:          [10u8; 16],
            inclusion_latency_mean: 500,
            confidence:             0.95,
        };
        let sim = fp.similarity(&fp);
        assert!((sim - 1.0).abs() < 1e-4, "identical fp must have similarity ~1.0, got {sim}");
    }

    #[test]
    fn actionable_above_threshold() {
        let fp = SearcherFingerprint {
            fingerprint_id: 1, relay_affinity: [0; 16],
            escalation_curve: [0.0; 8], retry_pattern: [0; 16],
            inclusion_latency_mean: 0, confidence: 0.80,
        };
        assert!(fp.is_actionable());
    }
}