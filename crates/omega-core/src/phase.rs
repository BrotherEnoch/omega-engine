// crates/omega-core/src/phase.rs
//! System phase model (§1.1, §20).
//!
//! `OmegaConfig::active_phase` is stored as `u8` for TOML compatibility.
//! This module provides a typed view and the strategy / relay gates that
//! every other crate should use instead of ad-hoc `active_phase >= n` checks.
//!
//! Phase meanings:
//!   0 — Shadow / backtest (canary signal validation; no capital deployment)
//!   1 — SA  (Simple Arbitrage)
//!   2 — MSA (Multi-Step Arbitrage)
//!   3 — LA  (Liquidation Arbitrage)
//!   4 — MEV (MEV-OFA / backrun) — full production
//!
//! Higher phases keep lower-phase strategies eligible (cumulative unlock).
//! Governance: changing active phase is L3 (48h timelock) in production.

use crate::types::StrategyId;
use serde::{Deserialize, Serialize};

/// Canonical system phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Phase {
    /// Shadow / backtest. Canary may run; no live capital strategies.
    Shadow = 0,
    /// Simple Arbitrage.
    Sa = 1,
    /// Multi-Step Arbitrage.
    Msa = 2,
    /// Liquidation Arbitrage.
    La = 3,
    /// MEV-OFA / backrun — full production.
    Mev = 4,
}

impl Phase {
    pub const MIN: Phase = Phase::Shadow;
    pub const MAX: Phase = Phase::Mev;

    /// Full production default (matches `config` defaults / Phase 4 ops).
    pub const PRODUCTION: Phase = Phase::Mev;

    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse a config `active_phase` value. Returns `None` if outside 0..=4.
    #[inline]
    pub const fn from_u8(value: u8) -> Option<Phase> {
        match value {
            0 => Some(Phase::Shadow),
            1 => Some(Phase::Sa),
            2 => Some(Phase::Msa),
            3 => Some(Phase::La),
            4 => Some(Phase::Mev),
            _ => None,
        }
    }

    /// Same as [`from_u8`], but clamps invalid values down to [`Phase::MAX`]
    /// is **not** done — invalid input becomes `Shadow` only via `from_u8_or_shadow`
    /// when a total function is required at a boundary.
    #[inline]
    pub fn from_u8_or_shadow(value: u8) -> Phase {
        Self::from_u8(value).unwrap_or(Phase::Shadow)
    }

    /// Human-readable label for logs / control-plane.
    pub const fn label(self) -> &'static str {
        match self {
            Phase::Shadow => "SHADOW",
            Phase::Sa => "SA",
            Phase::Msa => "MSA",
            Phase::La => "LA",
            Phase::Mev => "MEV",
        }
    }

    /// Whether `strategy` may activate at this system phase (§1.1).
    ///
    /// Delegates to [`StrategyId::phase_required`]: strategy is allowed when
    /// `active_phase >= strategy.phase_required()`.
    #[inline]
    pub fn allows_strategy(self, strategy: StrategyId) -> bool {
        self.as_u8() >= strategy.phase_required()
    }

    /// Whether this phase is full production (MEV gate open).
    #[inline]
    pub const fn is_production(self) -> bool {
        matches!(self, Phase::Mev)
    }

    /// Whether live capital strategies (non-canary) may run.
    #[inline]
    pub const fn allows_live_capital(self) -> bool {
        self.as_u8() >= Phase::Sa as u8
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl TryFrom<u8> for Phase {
    type Error = InvalidPhase;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Phase::from_u8(value).ok_or(InvalidPhase(value))
    }
}

impl From<Phase> for u8 {
    fn from(value: Phase) -> Self {
        value.as_u8()
    }
}

/// `active_phase` was outside the valid range 0..=4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPhase(pub u8);

impl std::fmt::Display for InvalidPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid active_phase {} — must be 0..=4", self.0)
    }
}

impl std::error::Error for InvalidPhase {}

/// Gate helpers that take the raw config `u8` without requiring a prior parse.
#[inline]
pub fn strategy_allowed_at_phase(active_phase: u8, strategy: StrategyId) -> bool {
    match Phase::from_u8(active_phase) {
        Some(p) => p.allows_strategy(strategy),
        None => false,
    }
}

/// Relays eligible for the given phase under `RelayConfig` lists.
///
/// Phase 0–1 use `phase_1_relays`; phase ≥ 2 use `phase_2plus_relays`
/// (see `RelayConfig` in `config.rs`).
#[inline]
pub fn relay_list_for_phase<'a>(
    active_phase: u8,
    phase_1_relays: &'a [String],
    phase_2plus_relays: &'a [String],
) -> &'a [String] {
    if active_phase >= 2 {
        phase_2plus_relays
    } else {
        phase_1_relays
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_round_trip_0_to_4() {
        for v in 0u8..=4 {
            let p = Phase::from_u8(v).expect("valid");
            assert_eq!(p.as_u8(), v);
        }
        assert!(Phase::from_u8(5).is_none());
    }

    #[test]
    fn production_is_mev() {
        assert_eq!(Phase::PRODUCTION, Phase::Mev);
        assert!(Phase::Mev.is_production());
        assert!(!Phase::La.is_production());
    }

    #[test]
    fn cumulative_strategy_gates() {
        // Phase 4 unlocks everything
        let p4 = Phase::Mev;
        assert!(p4.allows_strategy(StrategyId::Cnry));
        assert!(p4.allows_strategy(StrategyId::Sa));
        assert!(p4.allows_strategy(StrategyId::Msa));
        assert!(p4.allows_strategy(StrategyId::La));
        assert!(p4.allows_strategy(StrategyId::Mev));

        // Phase 1: CNRY + SA only
        let p1 = Phase::Sa;
        assert!(p1.allows_strategy(StrategyId::Cnry));
        assert!(p1.allows_strategy(StrategyId::Sa));
        assert!(!p1.allows_strategy(StrategyId::Msa));
        assert!(!p1.allows_strategy(StrategyId::La));
        assert!(!p1.allows_strategy(StrategyId::Mev));

        // Phase 0: canary only
        let p0 = Phase::Shadow;
        assert!(p0.allows_strategy(StrategyId::Cnry));
        assert!(!p0.allows_strategy(StrategyId::Sa));
    }

    #[test]
    fn strategy_allowed_at_phase_rejects_invalid_u8() {
        assert!(!strategy_allowed_at_phase(9, StrategyId::Sa));
    }

    #[test]
    fn relay_list_switches_at_phase_2() {
        let p1 = vec!["flashbots".into()];
        let p2 = vec!["titan".into()];
        assert_eq!(relay_list_for_phase(1, &p1, &p2), p1.as_slice());
        assert_eq!(relay_list_for_phase(2, &p1, &p2), p2.as_slice());
        assert_eq!(relay_list_for_phase(4, &p1, &p2), p2.as_slice());
    }

    #[test]
    fn try_from_u8() {
        assert_eq!(Phase::try_from(4u8).unwrap(), Phase::Mev);
        assert!(Phase::try_from(7u8).is_err());
    }
}
