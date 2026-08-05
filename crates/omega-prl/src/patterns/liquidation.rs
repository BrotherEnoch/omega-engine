// omega-prl/src/patterns/liquidation.rs
//! Liquidation Opportunity Patterning — §11

/// §11.2 — Per-position liquidation risk signature.
#[derive(Debug, Clone)]
pub struct LiquidationPattern {
    pub position_key: u64,
    pub hf_velocity: f32,
    pub oracle_correlation: f32,
    pub gas_pressure: f32,
    pub competitor_density: f32,
    pub expected_profit_decay: f32,
    pub confidence: f32,
}

impl LiquidationPattern {
    /// Composite urgency score [0, 1].  Higher = execute sooner.
    #[inline]
    pub fn urgency_score(&self) -> f32 {
        (0.40 * self.hf_velocity.abs()
            + 0.25 * self.oracle_correlation
            + 0.20 * self.gas_pressure
            + 0.15 * self.competitor_density)
            .clamp(0.0, 1.0)
    }

    /// §11.3 — Whether PRL recommends elevated recompute cadence.
    #[inline]
    pub fn should_elevate_priority(&self) -> bool {
        self.urgency_score() > 0.7 && self.confidence > 0.70
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_in_bounds() {
        let lp = LiquidationPattern {
            position_key: 1,
            hf_velocity: 0.9,
            oracle_correlation: 0.8,
            gas_pressure: 0.5,
            competitor_density: 0.6,
            expected_profit_decay: 0.2,
            confidence: 0.9,
        };
        let u = lp.urgency_score();
        // FIX: manual_range_contains → use RangeInclusive::contains
        assert!((0.0..=1.0).contains(&u), "urgency out of bounds: {u}");
    }

    #[test]
    fn high_urgency_elevates_priority() {
        let lp = LiquidationPattern {
            position_key: 1,
            hf_velocity: 1.0,
            oracle_correlation: 1.0,
            gas_pressure: 1.0,
            competitor_density: 1.0,
            expected_profit_decay: 0.0,
            confidence: 0.95,
        };
        assert!(lp.should_elevate_priority());
    }
}
