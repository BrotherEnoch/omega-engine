// crates/omega-risk/src/competition.rs
//
// Probabilistic competition model for LA blueprints (spec S11/S12).
//
// Two outputs consumed by the risk layer:
//
//   1. `competition_probability()` — probability that a competing bot wins [0.0, 1.0].
//      Drives check 11 (MISS_COMPETITION drop code).
//
//   2. `priority_fee_gwei()` — suggested priority fee in gwei for the Gas War Engine.
//      Drives adaptive gas cap (spec S12): 5 % of bonus_eth / 21_000 × urgency × win_rate.
//
// Asset tiers (spec: "Major/Mid/LongTail"):
//   Major    — WETH, WBTC: base competition = 85 %.
//   Mid      — LINK, UNI, AAVE, etc.: base = 60 %.
//   LongTail — everything else: base = 25 %.
//
// Health-factor urgency multipliers (spec S11):
//   HF < 1.001 → 3.0×   (imminent — every bot will fire)
//   HF < 1.005 → 2.0×
//   HF < 1.01  → 1.5×
//   HF ≥ 1.01  → 1.0×
//
// Size multipliers: larger liquidations attract more competition.
//
// Priority fee urgency × win_rate multipliers match the cascade_mode spec.
//
// Thread-safety: all functions are pure / stateless.

use serde::{Deserialize, Serialize};

// ─── Asset tier ───────────────────────────────────────────────────────────────

/// Asset tier classification (spec S11 competition model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetTier {
    /// WETH, WBTC — base competition 85 %.
    Major,
    /// LINK, UNI, AAVE, CRV, etc. — base competition 60 %.
    Mid,
    /// Long-tail — base competition 25 %.
    LongTail,
}

impl AssetTier {
    /// Classify by token symbol. Caller may override with on-chain data.
    pub fn from_symbol(symbol: &str) -> Self {
        match symbol.to_uppercase().as_str() {
            "WETH" | "WBTC" | "ETH" | "BTC" => AssetTier::Major,
            "LINK" | "UNI" | "AAVE" | "CRV" | "SNX" | "MKR" | "COMP" | "BAL" | "YFI" | "SUSHI"
            | "USDC" | "USDT" | "DAI" | "FRAX" | "LUSD" => AssetTier::Mid,
            _ => AssetTier::LongTail,
        }
    }

    /// Base competition probability before urgency and size adjustments.
    pub fn base_competition(self) -> f64 {
        match self {
            AssetTier::Major => 0.85,
            AssetTier::Mid => 0.60,
            AssetTier::LongTail => 0.25,
        }
    }
}

// ─── Competition probability ──────────────────────────────────────────────────

/// Urgency multiplier based on health factor (spec S11).
fn hf_urgency_multiplier(health_factor: f64) -> f64 {
    if health_factor < 1.001 {
        1.5
    } else if health_factor < 1.005 {
        1.2
    } else if health_factor < 1.01 {
        1.0
    } else {
        0.9
    }
}

/// Size multiplier — larger liquidations attract more bots (spec: log10 scaling).
fn size_multiplier(liquidation_eth: f64) -> f64 {
    if liquidation_eth <= 0.0 {
        return 1.0;
    }
    (1.0 + liquidation_eth.log10().max(0.0) * 0.1).min(1.5)
}

/// Compute the probability that a competing bot wins this liquidation [0.0, 0.99].
///
/// Used in check 11: if competition_probability > ctx.max_competition_probability
/// the blueprint is dropped with MISS_COMPETITION.
///
/// # Arguments
/// * `asset_tier`          — asset classification
/// * `health_factor`       — current HF of the target position
/// * `liquidation_eth`     — estimated liquidation size in ETH
pub fn competition_probability(
    asset_tier: AssetTier,
    health_factor: f64,
    liquidation_eth: f64,
) -> f64 {
    let base = asset_tier.base_competition();
    let urgency = hf_urgency_multiplier(health_factor);
    let size = size_multiplier(liquidation_eth);

    (base * urgency * size).min(0.99)
}

// ─── Priority fee computation ─────────────────────────────────────────────────

/// Urgency multiplier for priority fee (spec S12: separate from competition model).
fn fee_urgency_multiplier(health_factor: f64) -> f64 {
    if health_factor < 1.001 {
        3.0
    } else if health_factor < 1.005 {
        2.0
    } else if health_factor < 1.01 {
        1.5
    } else {
        1.0
    }
}

/// Win-rate multiplier: losing bots must bid higher to improve odds (spec S12).
fn win_rate_multiplier(historical_win_rate: f64) -> f64 {
    if historical_win_rate < 0.30 {
        1.8
    } else if historical_win_rate < 0.50 {
        1.3
    } else {
        1.0
    }
}

/// Compute suggested priority fee in gwei (spec S12 adaptive cap).
///
/// Formula: 5 % of expected_bonus_eth / 21_000 (gas for a simple transfer)
///          × urgency multiplier × win-rate multiplier,
///          clamped to [2 gwei, 500 gwei] (spec Arbitrum note I3).
///
/// # Arguments
/// * `expected_bonus_eth`  — expected liquidation bonus in ETH
/// * `health_factor`       — current HF
/// * `historical_win_rate` — rolling 30-day win rate for this strategy/asset
pub fn priority_fee_gwei(
    expected_bonus_eth: f64,
    health_factor: f64,
    historical_win_rate: f64,
) -> u64 {
    // 5 % of bonus / 21_000 gas (minimum profitable gas unit cost).
    let base_cap = (expected_bonus_eth * 1e9 * 0.05 / 21_000.0).max(0.0) as u64;

    let urgency = fee_urgency_multiplier(health_factor);
    let win_mult = win_rate_multiplier(historical_win_rate);

    let raw = (base_cap as f64 * urgency * win_mult) as u64;

    // Spec: [2 gwei, 500 gwei] on Arbitrum.
    raw.clamp(2, 500)
}

#[cfg(test)]
mod competition_tests {
    use super::*;

    // ── AssetTier::from_symbol ────────────────────────────────────────────────

    #[test]
    fn weth_is_major() {
        assert_eq!(AssetTier::from_symbol("WETH"), AssetTier::Major);
    }

    #[test]
    fn link_is_mid() {
        assert_eq!(AssetTier::from_symbol("LINK"), AssetTier::Mid);
    }

    #[test]
    fn unknown_is_longtail() {
        assert_eq!(AssetTier::from_symbol("OBSCURECOIN"), AssetTier::LongTail);
    }

    // ── competition_probability ───────────────────────────────────────────────

    #[test]
    fn major_asset_imminent_hf_high_probability() {
        let p = competition_probability(AssetTier::Major, 1.0005, 50.0);
        // base 0.85 × urgency 1.5 × size > 1 → capped at 0.99
        assert!(p > 0.90 && p <= 0.99, "expected high prob, got {}", p);
    }

    #[test]
    fn longtail_safe_hf_low_probability() {
        let p = competition_probability(AssetTier::LongTail, 1.10, 0.5);
        assert!(p < 0.30, "expected low prob, got {}", p);
    }

    #[test]
    fn probability_capped_at_0_99() {
        let p = competition_probability(AssetTier::Major, 1.0001, 1000.0);
        assert!(p <= 0.99);
    }

    #[test]
    fn probability_non_negative() {
        let p = competition_probability(AssetTier::LongTail, 1.50, 0.001);
        assert!(p >= 0.0);
    }

    // ── priority_fee_gwei ─────────────────────────────────────────────────────

    #[test]
    fn fee_floor_at_2_gwei() {
        // Zero bonus → base_cap = 0 → floor kicks in.
        let fee = priority_fee_gwei(0.0, 1.10, 0.90);
        assert_eq!(fee, 2);
    }

    #[test]
    fn fee_ceiling_at_500_gwei() {
        // 100 ETH bonus → enormous base → ceiling kicks in.
        let fee = priority_fee_gwei(100.0, 1.0001, 0.10);
        assert_eq!(fee, 500);
    }

    #[test]
    fn imminent_hf_raises_fee_vs_safe_hf() {
        let low = priority_fee_gwei(1.0, 1.10, 0.60);
        let high = priority_fee_gwei(1.0, 1.0001, 0.60);
        assert!(high >= low, "imminent HF should not lower fee");
    }

    #[test]
    fn low_win_rate_raises_fee() {
        let good_rate = priority_fee_gwei(0.1, 1.02, 0.80);
        let bad_rate = priority_fee_gwei(0.1, 1.02, 0.20);
        assert!(bad_rate >= good_rate);
    }
}
