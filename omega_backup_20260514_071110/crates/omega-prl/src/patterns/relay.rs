// omega-prl/src/patterns/relay.rs
//! Relay Intelligence Engine â€” Â§10

/// Â§10.2 â€” Per-relay health and trust scoring.
#[derive(Debug, Clone, Copy)]
pub struct RelayScore {
    pub inclusion_rate:      f32,
    pub median_latency_us:   u32,
    pub failure_rate:        f32,
    pub suspected_leak_rate: f32,
    pub censorship_score:    f32,
    /// Composite trust score [0, 1] â€” used for relay ranking and cascade order.
    pub trust_score:         f32,
}

impl RelayScore {
    /// Compute composite trust score.
    /// Weights: inclusion 40%, latency 20%, failure 10%, leak âˆ’30%, censorship âˆ’10%.
    #[inline]
    pub fn compute_trust(
        inclusion_rate:      f32,
        median_latency_us:   u32,
        failure_rate:        f32,
        suspected_leak_rate: f32,
        censorship_score:    f32,
    ) -> f32 {
        let latency_score = 1.0 - (median_latency_us as f32 / 10_000.0).clamp(0.0, 1.0);
        (0.40 * inclusion_rate
            + 0.20 * latency_score
            + 0.10 * (1.0 - failure_rate)
            - 0.30 * suspected_leak_rate
            - 0.10 * censorship_score)
            .clamp(0.0, 1.0)
    }

    pub fn new_observed(
        inclusion_rate:      f32,
        median_latency_us:   u32,
        failure_rate:        f32,
        suspected_leak_rate: f32,
        censorship_score:    f32,
    ) -> Self {
        Self {
            inclusion_rate,
            median_latency_us,
            failure_rate,
            suspected_leak_rate,
            censorship_score,
            trust_score: Self::compute_trust(
                inclusion_rate, median_latency_us, failure_rate,
                suspected_leak_rate, censorship_score,
            ),
        }
    }

    /// Â§10.3 â€” Whether this relay meets degradation thresholds.
    pub fn is_degraded(&self, baseline_inclusion: f32, baseline_latency_us: u32) -> bool {
        (baseline_inclusion - self.inclusion_rate) > 0.15
            || self.median_latency_us > baseline_latency_us * 2
    }

    /// Â§10.3 â€” Leak suspicion above 3Ïƒ normalised threshold.
    pub fn is_leak_suspected(&self) -> bool {
        self.suspected_leak_rate > 0.8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_score_in_bounds() {
        let s = RelayScore::new_observed(0.9, 100, 0.01, 0.0, 0.0);
        assert!(s.trust_score >= 0.0 && s.trust_score <= 1.0);
        assert!(s.trust_score > 0.5);
    }

    #[test]
    fn degraded_on_inclusion_drop() {
        let s = RelayScore::new_observed(0.60, 100, 0.01, 0.0, 0.0);
        assert!(s.is_degraded(0.80, 200));
    }

    #[test]
    fn leak_suspected_above_threshold() {
        let s = RelayScore::new_observed(0.9, 100, 0.01, 0.9, 0.0);
        assert!(s.is_leak_suspected());
    }
}