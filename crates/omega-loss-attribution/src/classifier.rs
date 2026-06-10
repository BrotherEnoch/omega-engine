// crates/omega-loss-attribution/src/classifier.rs
//
// Loss taxonomy and event types for the Loss Attribution Engine (§13).
//
// ## Spec §13 — Loss Attribution Engine
//
// Every blueprint that fails to execute on-chain is classified with one
// `LossCode`.  The Loss Attribution Engine aggregates codes into per-
// `FeatureKey` loss rates that drive the online ML fee multiplier model.
//
// ## Spec §13.4 — Simulation error sub-classification (fix M3)
//
// v11 used a single LOST_SIMULATION_ERROR code.  v12 sub-classifies
// simulation errors into three types with distinct root causes and
// corrective feedback actions:
//
//   SimulationStateMismatch   — stale revm cache; reduce trust window
//   SimulationExecutionRevert — calldata bug; CRITICAL alert + circuit breaker
//   SimulationGasMiscalc      — gas underestimate; increase gas_estimate_buffer
//
// ## Alignment with omega-core DropCode
//
// `LossCode` and `omega_core::DropCode` cover overlapping ground:
//   - `DropCode` is the pipeline-level discard reason (always recorded)
//   - `LossCode` is the ML training signal (recorded for attributable losses)
//
// The mapping is deliberate — not every DropCode produces a LossCode
// (e.g. `MissExpiry` is a clock race, not an attributable loss), and
// not every LossCode has a 1-to-1 DropCode counterpart (e.g.
// `LostRaceSameFee` is diagnosed post-hoc from on-chain data).
//
// ## blueprint_hash type
//
// `blueprint_hash` is `B256` (alloy-primitives), matching the field type
// on `ExecutionBlueprint`.  The original `[u8; 32]` was an arbitrary
// byte array with no type safety; `B256` enforces the correct domain.

use alloy_primitives::B256;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// LossCode
// ─────────────────────────────────────────────────────────────────────────────

/// 10-class loss taxonomy for the online ML fee model (§13, §13.4).
///
/// ## ML feedback mapping
///
/// | Code                       | ML action                                 |
/// |----------------------------|-------------------------------------------|
/// | LostGasLow                 | Increase multiplier for FeatureKey        |
/// | LostGasOverbid             | Decrease multiplier for FeatureKey        |
/// | Simulation*                | Sub-classified feedback (§13.4)           |
/// | All others                 | No multiplier change — informational      |
///
/// Only `LostGasLow` and `LostGasOverbid` directly update the fee
/// multiplier.  Simulation errors are routed to the sub-classified
/// feedback paths described in §13.4.  All other codes are aggregated
/// for observability but do not alter the ML model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LossCode {
    /// Another bot submitted before us — latency loss.
    /// No multiplier adjustment: the fix is latency, not fee.
    LostLatency,

    /// Our priority fee was too low — competitor's bundle was chosen.
    ///
    /// Feedback: increase fee multiplier for this `FeatureKey` (§13).
    LostGasLow,

    /// Bundle was included but net profit was negative — we overbid.
    ///
    /// Feedback: decrease fee multiplier for this `FeatureKey` (§13).
    LostGasOverbid,

    /// Another bot chose better collateral on the same position.
    LostWrongCollateral,

    /// v12 M3: simulation passed using stale revm cache; on-chain state
    /// differed.  Root cause: EIL double-buffer staleness (§6, §13.4).
    ///
    /// Corrective action: reduce revm trust window from 2 blocks to 1.
    SimulationStateMismatch,

    /// v12 M3: simulation passed but on-chain execution reverted —
    /// calldata bug in the strategy contract (§13.4).
    ///
    /// Corrective action: CRITICAL alert + strategy circuit breaker.
    /// Engineering review required before re-enabling.
    SimulationExecutionRevert,

    /// v12 M3: simulation underestimated actual gas; profit margin was
    /// consumed by the gas overage (§13.4).
    ///
    /// Corrective action: increase `gas_estimate_buffer` for this
    /// strategy + protocol combination.
    SimulationGasMiscalc,

    /// Same fee as competitor — block builder chose their bundle.
    /// No multiplier adjustment: outcome is non-deterministic at equal fees.
    LostRaceSameFee,

    /// Position was liquidated before we scored it — detection gap.
    /// No multiplier adjustment: the fix is faster oracle polling.
    MissedDetection,

    /// Aave eMode grace period prevented liquidation.
    /// No multiplier adjustment: protocol-level constraint.
    MissedGracePeriod,
}

impl LossCode {
    /// Returns `true` for simulation-stage losses (§13.4).
    ///
    /// Simulation losses are routed to their sub-classified feedback
    /// paths; they do NOT update the fee multiplier directly.
    #[inline]
    pub fn is_simulation_error(self) -> bool {
        matches!(
            self,
            LossCode::SimulationStateMismatch
                | LossCode::SimulationExecutionRevert
                | LossCode::SimulationGasMiscalc
        )
    }

    /// Returns `true` when this loss code should trigger a CRITICAL alert
    /// and circuit-breaker action (§13.4).
    #[inline]
    pub fn is_critical(self) -> bool {
        matches!(self, LossCode::SimulationExecutionRevert)
    }

    /// Returns `true` when this code should update the fee multiplier.
    ///
    /// Only `LostGasLow` and `LostGasOverbid` drive multiplier updates.
    #[inline]
    pub fn affects_multiplier(self) -> bool {
        matches!(self, LossCode::LostGasLow | LossCode::LostGasOverbid)
    }

    /// Canonical SCREAMING_SNAKE_CASE label for Prometheus and ELK payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            LossCode::LostLatency => "LOST_LATENCY",
            LossCode::LostGasLow => "LOST_GAS_LOW",
            LossCode::LostGasOverbid => "LOST_GAS_OVERBID",
            LossCode::LostWrongCollateral => "LOST_WRONG_COLLATERAL",
            LossCode::SimulationStateMismatch => "SIMULATION_STATE_MISMATCH",
            LossCode::SimulationExecutionRevert => "SIMULATION_EXECUTION_REVERT",
            LossCode::SimulationGasMiscalc => "SIMULATION_GAS_MISCALC",
            LossCode::LostRaceSameFee => "LOST_RACE_SAME_FEE",
            LossCode::MissedDetection => "MISSED_DETECTION",
            LossCode::MissedGracePeriod => "MISSED_GRACE_PERIOD",
        }
    }
}

impl std::fmt::Display for LossCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FeatureKey
// ─────────────────────────────────────────────────────────────────────────────

/// The ML model's grouping key for fee multiplier learning (§13).
///
/// Fee multipliers are stored per `FeatureKey` so that different
/// (asset, urgency, protocol, size) combinations learn independently.
/// A position liquidating 0.1 ETH of WETH on Aave has different gas
/// competition dynamics from a 500 ETH LINK position on Morpho.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FeatureKey {
    /// Asset tier (0 = blue-chip, 1 = mid-tier, 2 = long-tail).
    pub asset_tier: u8,
    /// Health factor urgency tier (0 = critical < 1.001, 1 = hot < 1.01,
    /// 2 = warm ≥ 1.01).  Matches §11.1 tier boundaries.
    pub hf_urgency: u8,
    /// Lending protocol identifier (canonical name: "aave_v3", "compound",
    /// "morpho", "euler_v2").
    pub protocol: String,
    /// Position size tier (0 = large > 100 ETH, 1 = mid > 10 ETH,
    /// 2 = small ≤ 10 ETH).
    pub size_tier: u8,
}

impl FeatureKey {
    /// Human-readable label for use in Prometheus metric labels and ELK.
    pub fn label(&self) -> String {
        format!(
            "asset{}|hf{}|{}|size{}",
            self.asset_tier, self.hf_urgency, self.protocol, self.size_tier
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FeatureKey classification helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Classify an asset symbol into a tier.
///
/// Tier 0 (blue-chip): WETH, WBTC — highest liquidity and tightest spreads.
/// Tier 1 (mid-tier):  LINK, UNI — moderate liquidity.
/// Tier 2 (long-tail): all others — lower liquidity, wider spreads.
///
/// This classification is intentionally static in v12.  Phase 4+ may
/// replace it with a dynamic oracle-driven tier based on 7-day volume.
pub fn asset_tier(asset: &str) -> u8 {
    match asset {
        "WETH" | "WBTC" => 0,
        "LINK" | "UNI" | "ARB" => 1,
        _ => 2,
    }
}

/// Classify a health factor into an urgency tier.
///
/// Thresholds match §11.1 hot/warm boundary:
///   0 = critical: HF < 1.001 (imminent liquidation)
///   1 = hot:      HF < 1.01  (LA hot tier)
///   2 = warm:     HF ≥ 1.01  (LA warm tier — competition less fierce)
pub fn hf_urgency_tier(hf: f64) -> u8 {
    if hf < 1.001 {
        0
    } else if hf < 1.01 {
        1
    } else {
        2
    }
}

/// Classify a liquidation size (ETH) into a size tier.
///
///   0 = large:  > 100 ETH
///   1 = mid:    > 10 ETH
///   2 = small:  ≤ 10 ETH
pub fn size_tier(eth: f64) -> u8 {
    if eth > 100.0 {
        0
    } else if eth > 10.0 {
        1
    } else {
        2
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LossEvent
// ─────────────────────────────────────────────────────────────────────────────

/// A single attributable loss event, consumed by the online ML learner (§13).
///
/// `blueprint_hash` is `B256` — the same type used on `ExecutionBlueprint`
/// — ensuring type-safe join across the pipeline.  The original `[u8; 32]`
/// was replaced because raw byte arrays provide no type safety and cannot
/// be formatted or compared using alloy conventions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LossEvent {
    /// Join key linking this event to the originating blueprint.
    /// Matches `ExecutionBlueprint::blueprint_hash`.
    pub blueprint_hash: B256,

    /// Loss classification (§13, §13.4).
    pub loss_code: LossCode,

    /// Priority fee we submitted in gwei.
    pub our_fee_gwei: u64,

    /// Priority fee the winning competitor submitted, if known.
    /// `None` for non-gas-competition losses (latency, grace period, etc.).
    pub competing_fee_gwei: Option<u64>,

    /// Asset symbol (e.g. "WETH", "WBTC", "LINK").
    pub asset: String,

    /// Lending protocol identifier (canonical: "aave_v3", "compound",
    /// "morpho", "euler_v2").
    pub protocol: String,

    /// Position health factor at the time the loss was observed.
    pub health_factor: f64,

    /// Liquidation size in ETH equivalent at execution time.
    pub liquidation_size_eth: f64,

    /// UTC timestamp when this loss was recorded.
    pub timestamp: DateTime<Utc>,
}

impl LossEvent {
    /// Derive the `FeatureKey` for this event's ML update.
    pub fn feature_key(&self) -> FeatureKey {
        FeatureKey {
            asset_tier: asset_tier(&self.asset),
            hf_urgency: hf_urgency_tier(self.health_factor),
            protocol: self.protocol.clone(),
            size_tier: size_tier(self.liquidation_size_eth),
        }
    }

    /// Deterministic holdout assignment: returns `true` for the
    /// validation set (20%) using the blueprint hash as entropy.
    ///
    /// Uses `blueprint_hash[0] % 5 == 0` — exactly 1 in 5 events go to
    /// the holdout set, matching the 20% split in §13.1 (fix C1).
    ///
    /// This is deterministic from the hash: the same event always lands
    /// in the same partition across restarts and replay.
    #[inline]
    pub fn is_holdout(&self) -> bool {
        self.blueprint_hash[0].is_multiple_of(5)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(hash_byte: u8, code: LossCode) -> LossEvent {
        let mut hash = B256::ZERO;
        hash.0[0] = hash_byte;
        LossEvent {
            blueprint_hash: hash,
            loss_code: code,
            our_fee_gwei: 100,
            competing_fee_gwei: Some(110),
            asset: "WETH".into(),
            protocol: "aave_v3".into(),
            health_factor: 1.005,
            liquidation_size_eth: 50.0,
            timestamp: Utc::now(),
        }
    }

    // ── LossCode classification ────────────────────────────────────────────

    #[test]
    fn simulation_errors_classified_correctly() {
        assert!(LossCode::SimulationStateMismatch.is_simulation_error());
        assert!(LossCode::SimulationExecutionRevert.is_simulation_error());
        assert!(LossCode::SimulationGasMiscalc.is_simulation_error());
        assert!(!LossCode::LostGasLow.is_simulation_error());
    }

    #[test]
    fn only_execution_revert_is_critical() {
        assert!(LossCode::SimulationExecutionRevert.is_critical());
        assert!(!LossCode::SimulationStateMismatch.is_critical());
        assert!(!LossCode::LostGasLow.is_critical());
    }

    #[test]
    fn only_gas_codes_affect_multiplier() {
        assert!(LossCode::LostGasLow.affects_multiplier());
        assert!(LossCode::LostGasOverbid.affects_multiplier());
        assert!(!LossCode::LostLatency.affects_multiplier());
        assert!(!LossCode::SimulationStateMismatch.affects_multiplier());
    }

    // ── FeatureKey classification ──────────────────────────────────────────

    #[test]
    fn asset_tier_classification() {
        assert_eq!(asset_tier("WETH"), 0);
        assert_eq!(asset_tier("WBTC"), 0);
        assert_eq!(asset_tier("LINK"), 1);
        assert_eq!(asset_tier("ARB"), 1);
        assert_eq!(asset_tier("USDC"), 2);
        assert_eq!(asset_tier(""), 2);
    }

    #[test]
    fn hf_urgency_tier_boundaries() {
        assert_eq!(hf_urgency_tier(1.0000), 0); // critical
        assert_eq!(hf_urgency_tier(1.0009), 0); // still critical
        assert_eq!(hf_urgency_tier(1.001), 1); // hot
        assert_eq!(hf_urgency_tier(1.009), 1); // hot
        assert_eq!(hf_urgency_tier(1.01), 2); // warm
        assert_eq!(hf_urgency_tier(1.5), 2); // warm
    }

    #[test]
    fn size_tier_boundaries() {
        assert_eq!(size_tier(200.0), 0); // large
        assert_eq!(size_tier(100.1), 0); // large
        assert_eq!(size_tier(100.0), 1); // mid (not > 100)
        assert_eq!(size_tier(50.0), 1); // mid
        assert_eq!(size_tier(10.1), 1); // mid
        assert_eq!(size_tier(10.0), 2); // small
        assert_eq!(size_tier(0.1), 2); // small
    }

    // ── Holdout split ─────────────────────────────────────────────────────

    #[test]
    fn holdout_is_deterministic() {
        let e = make_event(0, LossCode::LostGasLow);
        // byte 0 % 5 == 0 → holdout
        assert!(e.is_holdout());

        let e2 = make_event(1, LossCode::LostGasLow);
        // byte 1 % 5 == 1 → training
        assert!(!e2.is_holdout());
    }

    #[test]
    fn holdout_rate_is_approximately_20_pct() {
        let holdout_count = (0u8..=255).filter(|&b| b % 5 == 0).count();
        // 256 / 5 ≈ 51 events → ~20%
        let rate = holdout_count as f64 / 256.0;
        assert!((rate - 0.20).abs() < 0.02, "holdout rate={rate:.3}");
    }

    // ── Feature key ───────────────────────────────────────────────────────

    #[test]
    fn feature_key_derived_correctly() {
        let e = make_event(0, LossCode::LostGasLow);
        let k = e.feature_key();
        assert_eq!(k.asset_tier, 0); // WETH
        assert_eq!(k.hf_urgency, 1); // 1.005 → hot
        assert_eq!(k.protocol, "aave_v3");
        assert_eq!(k.size_tier, 1); // 50 ETH → mid
    }
}
