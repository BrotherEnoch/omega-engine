// omega-prl/src/patterns/gas_war.rs
//! Gas War Predictive Intelligence â€” Â§13

/// Â§13.2 â€” Gas war bid curve forecast.
#[derive(Debug, Clone)]
pub struct GasWarForecast {
    pub expected_clearing_fee:        u64,
    pub escalation_velocity:          f32,
    pub competitor_count_estimate:    u16,
    pub inclusion_probability:        f32,
    pub confidence:                   f32,
    /// Â§13.3: PRL may recommend emergency bundle but NEVER bypasses cap_gwei.
    pub emergency_bundle_recommended: bool,
}

impl GasWarForecast {
    /// Derive a forecast from recent fee observations.
    ///
    /// `cap_gwei` â€” governance ceiling.  Forecast NEVER recommends exceeding it.
    pub fn from_observed(recent_fees: &[u64], competitor_count: u16, cap_gwei: u64) -> Self {
        if recent_fees.is_empty() {
            return Self::default();
        }
        let n        = recent_fees.len();
        let last     = recent_fees[n - 1];
        let first    = recent_fees[0];
        let velocity = if n > 1 && first > 0 {
            (last as f64 - first as f64) / (n - 1) as f64
        } else {
            0.0
        };
        let expected = (last as f64 + velocity).clamp(0.0, cap_gwei as f64) as u64;
        let inclusion_prob =
            (1.0 / (1.0 + competitor_count as f32 * 0.15)).clamp(0.0, 1.0);
        // Emergency only if very high velocity AND headroom below cap (Â§13.3).
        let emergency = velocity > 50.0 && expected < cap_gwei;
        let confidence = ((n as f32).min(100.0) / 100.0).clamp(0.3, 0.99);
        Self {
            expected_clearing_fee:        expected,
            escalation_velocity:          velocity as f32,
            competitor_count_estimate:    competitor_count,
            inclusion_probability:        inclusion_prob,
            confidence,
            emergency_bundle_recommended: emergency,
        }
    }
}

impl Default for GasWarForecast {
    fn default() -> Self {
        Self {
            expected_clearing_fee:        0,
            escalation_velocity:          0.0,
            competitor_count_estimate:    0,
            inclusion_probability:        0.5,
            confidence:                   0.3,
            emergency_bundle_recommended: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_gwei_always_respected() {
        let fees     = vec![100u64, 200, 400, 800];
        let forecast = GasWarForecast::from_observed(&fees, 3, 500);
        assert!(forecast.expected_clearing_fee <= 500,
            "must respect cap_gwei; got {}", forecast.expected_clearing_fee);
    }

    #[test]
    fn empty_returns_default() {
        let f = GasWarForecast::from_observed(&[], 0, 500);
        assert_eq!(f.expected_clearing_fee, 0);
    }
}