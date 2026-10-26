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
// ## Fix (this revision): hf_urgency_multiplier values were wrong
//
// The spec doc comment always stated 3.0×/2.0×/1.5×/1.0×. The
// implementation had 1.5/1.2/1.0/0.9 — mismatched at every tier. The
// practical effect: for Major assets (base = 0.85) at any HF < 1.005:
//
//   old: 0.85 × 1.2 × size_mult ≥ 1.02 → clamped 0.99 > any sane threshold
//   old: 0.85 × 1.5 × size_mult ≥ 1.27 → clamped 0.99 > any sane threshold
//
// The correct values produce the same cap-out for imminent Major-asset
// liquidations, which is correct (they ARE highly contested), but the 1.0×
// tier for HF ≥ 1.01 now genuinely reflects lower urgency rather than
// the old 0.9× which could produce misleadingly LOW probabilities for
// safe-but-real liquidations that do attract competition.
//
// The HF < 1.001 tier changed from 1.5× to 3.0×; the observable difference
// is only at Mid (0.60 × 3.0 × 1.0 = 1.80 → 0.99) vs (0.60 × 1.5 = 0.90
// still → 0.99 after size) so in practice both old and new cap out for Mid
// at imminent HF — the fix matters most for the ≥1.01 tier where the old
// 0.9× depressed competition estimates below a credible floor.
//
// ## Non-LA strategy defaults
//
// SA/MSA/MEV strategies score pure DEX arbitrage, not target-position
// liquidations. They have no health-factor or per-position liquidation
// size — calling `competition_probability()` requires both. Added
// `StrategyCompetitionClass` and `non_la_competition_probability()` so
// callers that lack CompetitionInputs (context_assembly.rs) can get a
// meaningful estimate rather than failing closed to 1.0, which
// deterministically drops every non-LA blueprint at check 11.
//
// Thread-safety: all functions are pure / stateless.

use serde::{Deserialize, Serialize};

// ─── Asset tier ──────────────────────────────────────────────────────────────

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

// ─── Strategy competition class (non-LA) ─────────────────────────────────────

/// Competition classification for non-LA strategies that lack a target
/// position and therefore cannot supply health-factor or liquidation-size
/// inputs to `competition_probability()`.
///
/// Used by `non_la_competition_probability()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrategyCompetitionClass {
    /// Simple Arbitrage (SA): two-hop DEX-to-DEX spread capture on major pairs.
    /// Base competition is meaningful but well below liquidation-bot density —
    /// major DEX arb is open to anyone with a relay but the opportunity window
    /// is wider than a liquidatable position.
    SimpleArbitrage,
    /// Multi-hop Sandwich Arbitrage (MSA): three-hop sandwich around a pending
    /// user swap. Highly contested because it is mempool-visible.
    SandwichArbitrage,
    /// MEV / backrun: generalized backrun of on-chain state change.
    /// Intermediate competition — tighter than SA, less than MSA.
    Mev,
}

impl StrategyCompetitionClass {
    /// Base competition probability for this class, before MEV-Share adjustment.
    ///
    /// These values are calibrated for Arbitrum One: the smaller searcher
    /// population on L2 versus Ethereum mainnet justifies lower base values than
    /// an equivalent Ethereum strategy would use.
    pub fn base_competition(self) -> f64 {
        match self {
            StrategyCompetitionClass::SimpleArbitrage => 0.35,
            StrategyCompetitionClass::SandwichArbitrage => 0.70,
            StrategyCompetitionClass::Mev => 0.50,
        }
    }
}

/// Competition probability for a non-LA strategy that has no target position.
///
/// Unlike `competition_probability()`, this takes only the strategy class and
/// the optional MEV-Share event count (from the activity tracker in main).
/// When `mev_share_events` is `0` the result is simply the class's base
/// competition level — no urgency or size adjustment applies.
///
/// # Arguments
/// * `class`             — strategy class
/// * `mev_share_events`  — count of competing bundles seen in the current
///   activity window (see `competition_with_mev_share`)
pub fn non_la_competition_probability(
    class: StrategyCompetitionClass,
    mev_share_events: u32,
) -> f64 {
    let base = class.base_competition();
    competition_with_mev_share(base, mev_share_events)
}

// ─── Competition probability ──────────────────────────────────────────────────

/// Urgency multiplier based on health factor (spec S11).
///
/// HF < 1.001 → 3.0×   (imminent — every bot fires)
/// HF < 1.005 → 2.0×
/// HF < 1.01  → 1.5×
/// HF ≥ 1.01  → 1.0×
///
/// ## Fix (this revision)
/// Previous values were 1.5/1.2/1.0/0.9 — inconsistent with both the spec
/// doc comment and the `fee_urgency_multiplier` sibling below, which always
/// had the correct 3.0/2.0/1.5/1.0 values. The 0.9× floor was the most
/// incorrect: a position that is technically liquidatable but not yet urgent
/// (HF ≥ 1.01) still has real competition; 0.9× was pushing the computed
/// probability below the base rate.
fn hf_urgency_multiplier(health_factor: f64) -> f64 {
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
/// For non-LA strategies that lack a target position, use
/// `non_la_competition_probability()` instead — calling this function with
/// fabricated health-factor or liquidation-size values produces meaningless
/// results and will always cap out for Major-asset inputs.
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

/// Blend tier-based competition probability with recent MEV-Share order-flow intensity.
///
/// `mev_share_events_in_window` is the count of competition-indicating MEV-Share
/// events observed in the last activity window (see main's MevShareActivityTracker).
/// Each event adds a small bump (capped) so a quiet stream does not dominate the
/// asset-tier model, while a burst of bundles pushes probability toward 0.99.
pub fn competition_with_mev_share(base_probability: f64, mev_share_events_in_window: u32) -> f64 {
    const PER_EVENT_BUMP: f64 = 0.02;
    const MAX_BUMP: f64 = 0.25;
    let bump = (mev_share_events_in_window as f64 * PER_EVENT_BUMP).min(MAX_BUMP);
    (base_probability + bump).clamp(0.0, 0.99)
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

    // ── hf_urgency_multiplier (regression guard for the fix) ──────────────────

    #[test]
    fn urgency_multiplier_matches_spec() {
        // HF < 1.001 → 3.0×
        assert!((hf_urgency_multiplier(1.0001) - 3.0).abs() < 1e-9);
        // HF < 1.005 → 2.0×
        assert!((hf_urgency_multiplier(1.002) - 2.0).abs() < 1e-9);
        // HF < 1.01 → 1.5×
        assert!((hf_urgency_multiplier(1.007) - 1.5).abs() < 1e-9);
        // HF ≥ 1.01 → 1.0× (was 0.9× — fixed)
        assert!((hf_urgency_multiplier(1.05) - 1.0).abs() < 1e-9);
        assert!((hf_urgency_multiplier(2.00) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn safe_hf_no_longer_below_base_rate() {
        // Before the fix, 0.9× could push the computed probability below the
        // base rate, making a safe-but-real liquidatable position look less
        // contested than one with no urgency multiplier at all. After the fix
        // the floor is 1.0× — competition probability never falls below base.
        let p_safe = competition_probability(AssetTier::Mid, 1.10, 10.0);
        assert!(
            p_safe >= AssetTier::Mid.base_competition(),
            "HF ≥ 1.01 must not push probability below base: got {p_safe}"
        );
    }

    // ── competition_probability ───────────────────────────────────────────────

    #[test]
    fn major_asset_imminent_hf_high_probability() {
        let p = competition_probability(AssetTier::Major, 1.0005, 50.0);
        // base 0.85 × urgency 3.0 × size > 1 → capped at 0.99
        assert!(p > 0.90 && p <= 0.99, "expected high prob, got {}", p);
    }

    #[test]
    fn longtail_safe_hf_low_probability() {
        let p = competition_probability(AssetTier::LongTail, 1.10, 0.5);
        // 0.25 × 1.0 × size_mult(0.5) = 0.25 × 1.0 × 1.0 = 0.25
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

    // ── non_la_competition_probability ───────────────────────────────────────

    #[test]
    fn sa_base_competition_is_35pct() {
        let p = non_la_competition_probability(StrategyCompetitionClass::SimpleArbitrage, 0);
        assert!((p - 0.35).abs() < 1e-9, "SA base should be 0.35, got {p}");
    }

    #[test]
    fn msa_base_competition_is_70pct() {
        let p = non_la_competition_probability(StrategyCompetitionClass::SandwichArbitrage, 0);
        assert!((p - 0.70).abs() < 1e-9, "MSA base should be 0.70, got {p}");
    }

    #[test]
    fn mev_base_competition_is_50pct() {
        let p = non_la_competition_probability(StrategyCompetitionClass::Mev, 0);
        assert!((p - 0.50).abs() < 1e-9, "MEV base should be 0.50, got {p}");
    }

    #[test]
    fn mev_share_bumps_non_la_probability() {
        let low = non_la_competition_probability(StrategyCompetitionClass::SimpleArbitrage, 0);
        let high = non_la_competition_probability(StrategyCompetitionClass::SimpleArbitrage, 5);
        assert!(
            high > low,
            "MEV-Share events must increase SA competition estimate"
        );
        assert!(high <= 0.99);
    }

    #[test]
    fn sa_competition_well_below_default_threshold_at_quiet_market() {
        // Regression guard: with no MEV-Share activity, SA's base 0.35 is
        // well under any reasonable max_competition_probability (0.90+).
        // This test proves SA blueprints will pass check 11 in a quiet
        // market once context_assembly.rs passes the real estimate instead
        // of failing closed to 1.0.
        let p = non_la_competition_probability(StrategyCompetitionClass::SimpleArbitrage, 0);
        assert!(
            p < 0.90,
            "SA should pass check 11 at quiet market (p={p}, threshold=0.90)"
        );
    }

    // ── competition_with_mev_share ────────────────────────────────────────────

    #[test]
    fn mev_share_bump_caps_and_monotonic() {
        let base = 0.70;
        assert!((competition_with_mev_share(base, 0) - base).abs() < 1e-12);
        assert!(competition_with_mev_share(base, 5) > base);
        assert!((competition_with_mev_share(base, 100) - 0.95).abs() < 1e-9); // 0.70+0.25
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
