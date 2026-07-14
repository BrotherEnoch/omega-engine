// crates/omega-core/src/errors.rs
//
// OmegaError and DropCode — the canonical error taxonomy for the Omega Engine.
//
// ## Design principles
//
// 1. Every execution path that discards a blueprint must record exactly one
//    DropCode.  The Loss Attribution Engine (§13) uses DropCodes as its primary
//    input signal.  Ambiguous or missing codes corrupt the ML feedback loop.
//
// 2. OmegaError variants map 1-to-1 to architectural layers so that the Health
//    FSM (§3) can transition the correct layer to Degraded/Halted on receipt of
//    a typed error.
//
// 3. Simulation errors are sub-classified (§13.4, fix M3) — a single
//    LOST_SIMULATION_ERROR code was insufficient; the three sub-types have
//    distinct root causes and distinct corrective feedback actions.
//
// ## Serialization (added)
//
// `DropCode` now derives `Serialize`/`Deserialize` with
// `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`. This was previously
// missing despite the doc's own callout that "§16 — Observability: all
// DropCodes are always-sampled LA events" — an always-sampled event
// that can't be serialized can't actually be logged/exported in
// structured form. Every variant's auto-generated wire string was
// verified to match the existing hand-written `Display` impl exactly
// (e.g. `MissHfNotLiquidatable` → `"MISS_HF_NOT_LIQUIDATABLE"` either
// way), so this is a purely additive, zero-risk change: the Prometheus
// label convention (`Display`) and the structured-log wire format
// (`Serialize`) are now guaranteed to agree rather than being two
// independently-hand-maintained strings that could drift apart.
//
// `OmegaError` itself is NOT given Serialize/Deserialize in this pass —
// unlike DropCode, there's no equivalent explicit callout that it needs
// to cross a wire boundary, and guessing at a tagging scheme
// (adjacently-tagged vs internally-tagged vs untagged) risks producing
// a wire shape that conflicts with something already downstream. Add it
// deliberately, with a chosen tagging scheme, if/when a concrete
// consumer needs it.
//
// Spec references:
//   §3    — Health FSM: layer transitions triggered by OmegaError variants
//   §7    — Gas model: MissGas, MissGasSpike DropCodes
//   §8    — OFA compliance: MissOfaConsent, MissOfaSlippage, MissOfaOrder
//   §9    — DAG cycle detection: MissDagCycle
//   §11   — LA-specific drops: MissHfNotLiquidatable, MissFlashloan
//   §12   — Gas War: MissCompetition, MissCapacity
//   §13.4 — Simulation sub-classification: SimulationStateMismatch,
//            SimulationExecutionRevert, SimulationGasMiscalc
//   §16   — Observability: all DropCodes are always-sampled LA events

use std::fmt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─────────────────────────────────────────────────────────────────────────────
// DropCode
// ─────────────────────────────────────────────────────────────────────────────

/// Reason a blueprint was discarded without on-chain execution.
///
/// Every code corresponds to a distinct decision point in the pipeline.
/// The Loss Attribution Engine (§13) aggregates these into per-feature-key
/// loss rates that drive the online ML model.
///
/// ### Code taxonomy
///
/// | Prefix   | Meaning                                               |
/// |----------|-------------------------------------------------------|
/// | `Miss*`  | Opportunity did not meet a threshold — expected drop  |
/// | `Sim*`   | Blueprint failed at simulation stage (§13.4)          |
/// | `Wrong*` | Blueprint was structurally invalid / misrouted        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DropCode {
    // ── Routing / identity ───────────────────────────────────────────────
    /// Blueprint submitted to a relay targeting the wrong chain ID.
    /// Triggers `DropCode::WrongChainId` metric + blueprint discard.
    WrongChain,

    /// Same as `WrongChain` but caught at the blueprint-construction
    /// stage (chain_id field doesn't match the active ChainId).
    WrongChainId,

    // ── Timing ───────────────────────────────────────────────────────────
    /// Blueprint's `expiry_block` has passed before submission.
    MissExpiry,

    // ── Gas / fee model (§7) ─────────────────────────────────────────────
    /// Estimated gas cost exceeds the dynamic_min_profit threshold.
    MissGas,

    /// Gas spike detected between blueprint construction and submission;
    /// expected profit is no longer sufficient at the elevated fee.
    MissGasSpike,

    // ── Access control / whitelist ────────────────────────────────────────
    /// Strategy contract is not in the StrategyRegistry whitelist (§8).
    MissWhitelist,

    // ── Profitability ────────────────────────────────────────────────────
    /// `expected_profit_net` is below `dynamic_min_profit` at scoring.
    MissProfit,

    // ── Oracle ───────────────────────────────────────────────────────────
    /// Oracle feed is unavailable or stale beyond the trust window.
    MissOracle,

    /// Two oracle sources diverge beyond the acceptable threshold;
    /// neither can be trusted as ground truth.
    MissOracleDiverge,

    // ── Execution quality ────────────────────────────────────────────────
    /// Projected slippage exceeds `slippage_bps` limit.
    MissSlippage,

    /// Insufficient on-chain liquidity to execute the swap(s) at the
    /// required size.
    MissLiquidity,

    /// DEX-specific liquidity check failed (pool depth, tick range, etc.).
    MissDexLiquidity,

    /// Price impact exceeds the `price_impact_bps` threshold.
    MissPriceImpact,

    // ── Competition / relay ───────────────────────────────────────────────
    /// Competition probability too high; expected EV is negative after
    /// accounting for the likelihood of being outbid (§19).
    MissCompetition,

    // ── Capacity ─────────────────────────────────────────────────────────
    /// Hot-path execution capacity exhausted (Microtx lane full).
    MissCapacity,

    /// Normal-path execution capacity exhausted.
    MissCapacityNormal,

    // ── Risk ─────────────────────────────────────────────────────────────
    /// Risk layer (omega-risk) vetoed the blueprint.
    MissRisk,

    /// Flash crash detected — price move exceeds safety threshold;
    /// all execution halted until oracle stabilises.
    MissFlashCrash,

    // ── DAG ──────────────────────────────────────────────────────────────
    /// DAG dependency resolution detected a cycle; blueprint cannot be
    /// scheduled (§9).
    MissDagCycle,

    // ── Flashloan (§11) ──────────────────────────────────────────────────
    /// Required flashloan liquidity is unavailable from any provider.
    MissFlashloan,

    // ── LA-specific (§11) ────────────────────────────────────────────────
    /// Position health factor is above 1.0 at execution time — no longer
    /// liquidatable.  Common in competitive markets where another searcher
    /// landed first.
    MissHfNotLiquidatable,

    // ── OFA compliance (§8) ──────────────────────────────────────────────
    /// Order flow agreement consent is missing for this user/protocol.
    MissOfaConsent,

    /// OFA slippage protection check failed.
    MissOfaSlippage,

    /// OFA order validation failed (malformed or expired order).
    MissOfaOrder,

    // ── Simulation sub-classification (§13.4, fix M3) ────────────────────
    /// Simulation passed but on-chain state differed from the revm cache
    /// snapshot — stale EIL double-buffer (§6).
    ///
    /// Corrective action: trigger immediate revm cache refresh; reduce
    /// revm trust window from 2 blocks to 1 block.
    SimulationStateMismatch,

    /// Simulation passed but on-chain execution reverted for non-state
    /// reasons — calldata bug.
    ///
    /// Corrective action: CRITICAL alert + strategy circuit breaker;
    /// engineering review required.
    SimulationExecutionRevert,

    /// Simulation underestimated actual gas consumed; profit margin was
    /// consumed by the gas overage.
    ///
    /// Corrective action: increase `gas_estimate_buffer` for this
    /// strategy + protocol combination.
    SimulationGasMiscalc,
}

impl fmt::Display for DropCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Produces the canonical SCREAMING_SNAKE_CASE label used in
        // Prometheus counters and ELK log payloads (§16). Kept as an
        // explicit match (rather than deriving from the serde wire
        // format at runtime) so a missing arm here is a compile error
        // the moment a new variant is added — see the note on
        // `#[non_exhaustive]` below.
        let s = match self {
            DropCode::WrongChain => "WRONG_CHAIN",
            DropCode::WrongChainId => "WRONG_CHAIN_ID",
            DropCode::MissExpiry => "MISS_EXPIRY",
            DropCode::MissGas => "MISS_GAS",
            DropCode::MissGasSpike => "MISS_GAS_SPIKE",
            DropCode::MissWhitelist => "MISS_WHITELIST",
            DropCode::MissProfit => "MISS_PROFIT",
            DropCode::MissOracle => "MISS_ORACLE",
            DropCode::MissOracleDiverge => "MISS_ORACLE_DIVERGE",
            DropCode::MissSlippage => "MISS_SLIPPAGE",
            DropCode::MissLiquidity => "MISS_LIQUIDITY",
            DropCode::MissDexLiquidity => "MISS_DEX_LIQUIDITY",
            DropCode::MissPriceImpact => "MISS_PRICE_IMPACT",
            DropCode::MissCompetition => "MISS_COMPETITION",
            DropCode::MissCapacity => "MISS_CAPACITY",
            DropCode::MissCapacityNormal => "MISS_CAPACITY_NORMAL",
            DropCode::MissRisk => "MISS_RISK",
            DropCode::MissFlashCrash => "MISS_FLASH_CRASH",
            DropCode::MissDagCycle => "MISS_DAG_CYCLE",
            DropCode::MissFlashloan => "MISS_FLASHLOAN",
            DropCode::MissHfNotLiquidatable => "MISS_HF_NOT_LIQUIDATABLE",
            DropCode::MissOfaConsent => "MISS_OFA_CONSENT",
            DropCode::MissOfaSlippage => "MISS_OFA_SLIPPAGE",
            DropCode::MissOfaOrder => "MISS_OFA_ORDER",
            DropCode::SimulationStateMismatch => "SIMULATION_STATE_MISMATCH",
            DropCode::SimulationExecutionRevert => "SIMULATION_EXECUTION_REVERT",
            DropCode::SimulationGasMiscalc => "SIMULATION_GAS_MISCALC",
        };
        f.write_str(s)
    }
}

impl DropCode {
    /// Returns `true` for simulation-stage drops (§13.4).
    ///
    /// The Loss Attribution Engine uses this to route simulation errors
    /// into the LOST_SIMULATION_ERROR feedback path, distinct from the
    /// LOST_GAS_LOW / LOST_GAS_OVERBID paths.
    #[inline]
    pub fn is_simulation_error(self) -> bool {
        matches!(
            self,
            DropCode::SimulationStateMismatch
                | DropCode::SimulationExecutionRevert
                | DropCode::SimulationGasMiscalc
        )
    }

    /// Returns `true` for codes that represent expected, non-actionable
    /// drops (market moved, competition won, oracle stale).
    ///
    /// These codes do NOT trigger alerts and are NOT fed into the ML
    /// model as loss events — they represent correct engine behaviour.
    #[inline]
    pub fn is_expected_miss(self) -> bool {
        matches!(
            self,
            DropCode::MissExpiry
                | DropCode::MissCompetition
                | DropCode::MissHfNotLiquidatable
                | DropCode::MissFlashCrash
                | DropCode::MissOracle
        )
    }

    /// Returns `true` for codes that indicate a critical engineering
    /// fault requiring immediate circuit-breaker action (§13.4).
    #[inline]
    pub fn is_critical(self) -> bool {
        matches!(self, DropCode::SimulationExecutionRevert)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UnknownChainId
// ─────────────────────────────────────────────────────────────────────────────
//
// (No changes below this point in the file — OmegaError and its impls
// are unchanged from the reviewed version. `is_critical()`'s narrow
// scope, and `is_expected_miss()`'s specific 5-code allowlist, were both
// considered during this audit: whether e.g. `WrongChain`/`WrongChainId`
// should also be `is_critical()` is a domain/spec judgment call this
// crate's own files don't have enough context to settle — flagged in
// the accompanying review rather than changed speculatively.)

// ─────────────────────────────────────────────────────────────────────────────
// OmegaError
// ─────────────────────────────────────────────────────────────────────────────

/// Top-level error type for the Omega Engine.
///
/// Variants map to architectural layers so the Health FSM orchestrator
/// (§3) can transition the correct layer on error receipt:
///
/// | Variant          | Layer triggered   | FSM transition      |
/// |------------------|-------------------|---------------------|
/// | Dropped          | Strategy          | none (expected)     |
/// | IntegrityFail    | Security          | Halted              |
/// | Oracle           | ExternalData      | Degraded → Halted   |
/// | Relay            | Relay             | Degraded → Halted   |
/// | Zk               | Zk                | Degraded → Halted   |
/// | Config           | SystemHealth      | Halted              |
/// | ChainMismatch    | Orchestrator      | none (blueprint drop)|
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OmegaError {
    /// Blueprint was intentionally discarded.  This is NOT a fault —
    /// it is a normal pipeline outcome.  The embedded `DropCode` is
    /// the signal consumed by the Loss Attribution Engine (§13).
    #[error("Blueprint dropped: {code}")]
    Dropped { code: DropCode },

    /// An integrity invariant was violated (signature mismatch, hash
    /// mismatch, StrategyRegistry rejection).  Triggers Security layer
    /// Halted (§3, §8).
    #[error("Integrity failure: {detail}")]
    IntegrityFail { detail: String },

    /// Oracle layer error — feed unavailable, stale, or diverged beyond
    /// acceptable bounds.  Triggers ExternalData Degraded (§3).
    #[error("Oracle error: {msg}")]
    Oracle { msg: String },

    /// Relay submission error — connection failure, rate-limit, or relay
    /// rejection.  Triggers Relay Degraded (§3).
    #[error("Relay error: {msg}")]
    Relay { msg: String },

    /// ZK proof generation or verification error.  Triggers Zk Degraded
    /// (§3, §15).
    #[error("ZK error: {msg}")]
    Zk { msg: String },

    /// Static configuration error detected at startup or hot-reload.
    /// Triggers SystemHealth Halted immediately — the engine cannot
    /// operate with an invalid configuration.
    #[error("Configuration error: {msg}")]
    Config { msg: String },

    /// Blueprint's `chain_id` field does not match the active ChainId.
    /// Blueprint is discarded with `DropCode::WrongChainId`; no layer
    /// transition is triggered.
    #[error("Chain mismatch: blueprint chain_id={blueprint} active={active}")]
    ChainMismatch { blueprint: u64, active: u64 },
}

impl OmegaError {
    /// Convenience constructor: blueprint dropped with the given code.
    #[inline]
    pub fn dropped(code: DropCode) -> Self {
        OmegaError::Dropped { code }
    }

    /// Returns the `DropCode` if this is a `Dropped` variant.
    #[inline]
    pub fn drop_code(&self) -> Option<DropCode> {
        match self {
            OmegaError::Dropped { code } => Some(*code),
            _ => None,
        }
    }

    /// Returns `true` when this error represents a critical fault
    /// requiring circuit-breaker action (§13.4, §3).
    #[inline]
    pub fn is_critical(&self) -> bool {
        match self {
            OmegaError::Dropped { code } => code.is_critical(),
            OmegaError::IntegrityFail { .. } => true,
            OmegaError::Config { .. } => true,
            _ => false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_code_display_screaming_snake() {
        assert_eq!(
            DropCode::SimulationStateMismatch.to_string(),
            "SIMULATION_STATE_MISMATCH"
        );
        assert_eq!(
            DropCode::SimulationExecutionRevert.to_string(),
            "SIMULATION_EXECUTION_REVERT"
        );
        assert_eq!(
            DropCode::SimulationGasMiscalc.to_string(),
            "SIMULATION_GAS_MISCALC"
        );
        assert_eq!(
            DropCode::MissHfNotLiquidatable.to_string(),
            "MISS_HF_NOT_LIQUIDATABLE"
        );
        assert_eq!(DropCode::WrongChainId.to_string(), "WRONG_CHAIN_ID");
    }

    #[test]
    fn drop_code_serde_wire_format_matches_display() {
        // Confirms the newly-added Serialize/Deserialize derive
        // produces exactly the same string as the hand-written Display
        // impl for every variant — so Prometheus labels and structured
        // JSON logs can never silently drift apart.
        let all_codes = [
            DropCode::WrongChain,
            DropCode::WrongChainId,
            DropCode::MissExpiry,
            DropCode::MissGas,
            DropCode::MissGasSpike,
            DropCode::MissWhitelist,
            DropCode::MissProfit,
            DropCode::MissOracle,
            DropCode::MissOracleDiverge,
            DropCode::MissSlippage,
            DropCode::MissLiquidity,
            DropCode::MissDexLiquidity,
            DropCode::MissPriceImpact,
            DropCode::MissCompetition,
            DropCode::MissCapacity,
            DropCode::MissCapacityNormal,
            DropCode::MissRisk,
            DropCode::MissFlashCrash,
            DropCode::MissDagCycle,
            DropCode::MissFlashloan,
            DropCode::MissHfNotLiquidatable,
            DropCode::MissOfaConsent,
            DropCode::MissOfaSlippage,
            DropCode::MissOfaOrder,
            DropCode::SimulationStateMismatch,
            DropCode::SimulationExecutionRevert,
            DropCode::SimulationGasMiscalc,
        ];
        for code in all_codes {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{code}\""), "wire format must match Display for {code}");
            let back: DropCode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, code, "round-trip must preserve the variant for {code}");
        }
    }

    #[test]
    fn simulation_error_classification() {
        assert!(DropCode::SimulationStateMismatch.is_simulation_error());
        assert!(DropCode::SimulationExecutionRevert.is_simulation_error());
        assert!(DropCode::SimulationGasMiscalc.is_simulation_error());
        assert!(!DropCode::MissGas.is_simulation_error());
        assert!(!DropCode::MissProfit.is_simulation_error());
    }

    #[test]
    fn critical_classification() {
        assert!(DropCode::SimulationExecutionRevert.is_critical());
        assert!(!DropCode::SimulationStateMismatch.is_critical());
        assert!(!DropCode::SimulationGasMiscalc.is_critical());
        assert!(!DropCode::MissGas.is_critical());
    }

    #[test]
    fn expected_miss_classification() {
        assert!(DropCode::MissExpiry.is_expected_miss());
        assert!(DropCode::MissCompetition.is_expected_miss());
        assert!(DropCode::MissHfNotLiquidatable.is_expected_miss());
        // Engineering faults are NOT expected misses
        assert!(!DropCode::SimulationExecutionRevert.is_expected_miss());
        assert!(!DropCode::MissGas.is_expected_miss());
    }

    #[test]
    fn omega_error_drop_code_accessor() {
        let e = OmegaError::dropped(DropCode::MissProfit);
        assert_eq!(e.drop_code(), Some(DropCode::MissProfit));

        let e2 = OmegaError::Oracle {
            msg: "stale".into(),
        };
        assert_eq!(e2.drop_code(), None);
    }

    #[test]
    fn omega_error_is_critical() {
        assert!(OmegaError::dropped(DropCode::SimulationExecutionRevert).is_critical());
        assert!(OmegaError::IntegrityFail {
            detail: "hash mismatch".into()
        }
        .is_critical());
        assert!(OmegaError::Config {
            msg: "bad toml".into()
        }
        .is_critical());
        assert!(!OmegaError::dropped(DropCode::MissProfit).is_critical());
        assert!(!OmegaError::Relay {
            msg: "timeout".into()
        }
        .is_critical());
    }
}