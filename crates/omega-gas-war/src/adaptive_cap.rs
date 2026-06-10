// crates/omega-gas-war/src/adaptive_cap.rs
// crates/omega-gas-war/src/adaptive_cap.rs
// (unchanged except line 352: manual_range_contains fix)

use std::fmt;

pub const GAS_PER_BUNDLE: f64 = 21_000.0;
pub const MAX_PRIORITY_FEE_GWEI: u64 = 500;
pub const MIN_PRIORITY_FEE_GWEI: u64 = 2;
const BONUS_GAS_FRACTION: f64 = 0.05;
const GWEI_PER_ETH: f64 = 1_000_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UrgencyTier {
    Critical,
    High,
    Moderate,
    Low,
}

impl UrgencyTier {
    pub fn from_health_factor(hf: f64) -> Self {
        if hf < 1.001 { UrgencyTier::Critical }
        else if hf < 1.005 { UrgencyTier::High }
        else if hf < 1.01  { UrgencyTier::Moderate }
        else               { UrgencyTier::Low }
    }

    pub fn multiplier(self) -> f64 {
        match self {
            UrgencyTier::Critical => 3.0,
            UrgencyTier::High     => 2.0,
            UrgencyTier::Moderate => 1.5,
            UrgencyTier::Low      => 1.0,
        }
    }
}

impl fmt::Display for UrgencyTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrgencyTier::Critical => f.write_str("CRITICAL"),
            UrgencyTier::High     => f.write_str("HIGH"),
            UrgencyTier::Moderate => f.write_str("MODERATE"),
            UrgencyTier::Low      => f.write_str("LOW"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WinRateTier {
    Low,
    Mid,
    High,
}

impl WinRateTier {
    pub fn from_rate(rate: f64) -> Self {
        if rate < 0.30      { WinRateTier::Low }
        else if rate < 0.50 { WinRateTier::Mid }
        else                { WinRateTier::High }
    }

    pub fn multiplier(self) -> f64 {
        match self {
            WinRateTier::Low  => 1.8,
            WinRateTier::Mid  => 1.3,
            WinRateTier::High => 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapComponents {
    pub base_cap_gwei:  u64,
    pub urgency_tier:   UrgencyTier,
    pub win_rate_tier:  WinRateTier,
    pub win_rate:       f64,
    pub final_cap_gwei: u64,
    pub clamped_to_max: bool,
}

pub fn adaptive_gas_cap_gwei(
    liquidation_bonus_eth: f64,
    health_factor:         f64,
    win_rate_fn:           impl Fn(u64) -> f64,
) -> (u64, CapComponents) {
    if liquidation_bonus_eth.is_nan() || liquidation_bonus_eth <= 0.0 {
        tracing::warn!(liquidation_bonus_eth,
            "adaptive_gas_cap: non-positive bonus — returning minimum cap");
        let components = CapComponents {
            base_cap_gwei:  MIN_PRIORITY_FEE_GWEI,
            urgency_tier:   UrgencyTier::from_health_factor(health_factor),
            win_rate_tier:  WinRateTier::High,
            win_rate:       1.0,
            final_cap_gwei: MIN_PRIORITY_FEE_GWEI,
            clamped_to_max: false,
        };
        return (MIN_PRIORITY_FEE_GWEI, components);
    }
    if liquidation_bonus_eth.is_infinite() {
        let urgency_tier  = UrgencyTier::from_health_factor(health_factor);
        let win_rate      = win_rate_fn(MAX_PRIORITY_FEE_GWEI).clamp(0.0, 1.0);
        let win_rate_tier = WinRateTier::from_rate(win_rate);
        let components = CapComponents {
            base_cap_gwei:  MAX_PRIORITY_FEE_GWEI,
            urgency_tier,
            win_rate_tier,
            win_rate,
            final_cap_gwei: MAX_PRIORITY_FEE_GWEI,
            clamped_to_max: true,
        };
        return (MAX_PRIORITY_FEE_GWEI, components);
    }

    let base_cap_f64  = (liquidation_bonus_eth * GWEI_PER_ETH * BONUS_GAS_FRACTION) / GAS_PER_BUNDLE;
    let base_cap_gwei = if base_cap_f64 < 0.0 || base_cap_f64.is_nan() {
        0_u64
    } else {
        base_cap_f64.min(u64::MAX as f64) as u64
    };

    let urgency_tier  = UrgencyTier::from_health_factor(health_factor);
    let urgency_mult  = urgency_tier.multiplier();
    let win_rate      = win_rate_fn(base_cap_gwei).clamp(0.0, 1.0);
    let win_rate_tier = WinRateTier::from_rate(win_rate);
    let wr_mult       = win_rate_tier.multiplier();

    let raw_cap_f64 = base_cap_f64 * urgency_mult * wr_mult;
    let raw_cap = if raw_cap_f64 < 0.0 || raw_cap_f64.is_nan() {
        0_u64
    } else {
        raw_cap_f64.min(u64::MAX as f64) as u64
    };

    let clamped        = raw_cap.clamp(MIN_PRIORITY_FEE_GWEI, MAX_PRIORITY_FEE_GWEI);
    let clamped_to_max = raw_cap > MAX_PRIORITY_FEE_GWEI;

    tracing::debug!(
        liquidation_bonus_eth, health_factor, base_cap_gwei,
        urgency_tier = %urgency_tier, win_rate,
        raw_cap_gwei = raw_cap, final_cap_gwei = clamped, clamped_to_max,
        "adaptive_gas_cap computed",
    );

    let components = CapComponents {
        base_cap_gwei, urgency_tier, win_rate_tier,
        win_rate, final_cap_gwei: clamped, clamped_to_max,
    };
    (clamped, components)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN_50: fn(u64) -> f64 = |_| 0.50;
    const WIN_20: fn(u64) -> f64 = |_| 0.20;
    const WIN_60: fn(u64) -> f64 = |_| 0.60;

    #[test]
    fn formula_unit_analysis() {
        let (cap, c) = adaptive_gas_cap_gwei(1.0, 1.05, WIN_50);
        assert_eq!(cap, MAX_PRIORITY_FEE_GWEI);
        assert!(c.clamped_to_max);
        assert_eq!(c.base_cap_gwei, 2_380);
    }

    #[test]
    fn small_bonus_low_hf_under_ceiling() {
        let (cap, c) = adaptive_gas_cap_gwei(0.001, 1.0005, WIN_20);
        assert_eq!(c.urgency_tier,   UrgencyTier::Critical);
        assert_eq!(c.win_rate_tier,  WinRateTier::Low);
        assert!(cap >= MIN_PRIORITY_FEE_GWEI);
        assert!(cap <= MAX_PRIORITY_FEE_GWEI);
        // FIX: manual_range_contains → use RangeInclusive::contains
        assert!((9..=12).contains(&cap), "cap={cap}");
    }

    #[test]
    fn always_at_most_500_gwei() {
        for bonus in [0.001, 0.1, 1.0, 10.0, 100.0] {
            for hf in [1.0, 1.0005, 1.003, 1.008, 1.05] {
                let (cap, _) = adaptive_gas_cap_gwei(bonus, hf, WIN_20);
                assert!(cap <= MAX_PRIORITY_FEE_GWEI, "cap={cap} bonus={bonus} hf={hf}");
            }
        }
    }

    #[test]
    fn always_at_least_2_gwei() {
        let (cap, _) = adaptive_gas_cap_gwei(0.000001, 1.5, WIN_60);
        assert!(cap >= MIN_PRIORITY_FEE_GWEI, "cap={cap}");
    }

    #[test]
    fn zero_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(0.0, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn negative_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(-1.0, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn nan_bonus_returns_minimum() {
        let (cap, _) = adaptive_gas_cap_gwei(f64::NAN, 1.0, WIN_50);
        assert_eq!(cap, MIN_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn infinite_bonus_clamps_to_max() {
        let (cap, _) = adaptive_gas_cap_gwei(f64::INFINITY, 1.0, WIN_50);
        assert_eq!(cap, MAX_PRIORITY_FEE_GWEI);
    }

    #[test]
    fn urgency_tiers() {
        assert_eq!(UrgencyTier::from_health_factor(1.0000), UrgencyTier::Critical);
        assert_eq!(UrgencyTier::from_health_factor(1.0009), UrgencyTier::Critical);
        assert_eq!(UrgencyTier::from_health_factor(1.001),  UrgencyTier::High);
        assert_eq!(UrgencyTier::from_health_factor(1.004),  UrgencyTier::High);
        assert_eq!(UrgencyTier::from_health_factor(1.005),  UrgencyTier::Moderate);
        assert_eq!(UrgencyTier::from_health_factor(1.009),  UrgencyTier::Moderate);
        assert_eq!(UrgencyTier::from_health_factor(1.01),   UrgencyTier::Low);
        assert_eq!(UrgencyTier::from_health_factor(1.5),    UrgencyTier::Low);
    }

    #[test]
    fn win_rate_tiers() {
        assert_eq!(WinRateTier::from_rate(0.00), WinRateTier::Low);
        assert_eq!(WinRateTier::from_rate(0.29), WinRateTier::Low);
        assert_eq!(WinRateTier::from_rate(0.30), WinRateTier::Mid);
        assert_eq!(WinRateTier::from_rate(0.49), WinRateTier::Mid);
        assert_eq!(WinRateTier::from_rate(0.50), WinRateTier::High);
        assert_eq!(WinRateTier::from_rate(1.00), WinRateTier::High);
    }

    #[test]
    fn higher_urgency_never_lower_cap() {
        let (cap_low,  _) = adaptive_gas_cap_gwei(0.05, 1.02,   WIN_50);
        let (cap_high, _) = adaptive_gas_cap_gwei(0.05, 1.0001, WIN_50);
        assert!(cap_high >= cap_low,
            "critical HF should produce cap ≥ low urgency: {cap_high} vs {cap_low}");
    }

    #[test]
    fn lower_win_rate_never_lower_cap() {
        let (cap_high, _) = adaptive_gas_cap_gwei(0.05, 1.05, WIN_20);
        let (cap_low,  _) = adaptive_gas_cap_gwei(0.05, 1.05, WIN_60);
        assert!(cap_high >= cap_low,
            "low win rate should produce cap ≥ high win rate: {cap_high} vs {cap_low}");
    }
}