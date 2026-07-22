ï»¿// crates/omega-core/src/errors.rs
//
// OmegaError and DropCode â€” the canonical error taxonomy for the Omega Engine.
//
// ## Design principles
//
// 1. Every execution path that discards a blueprint must record exactly one
//    DropCode.  The Loss Attribution Engine (Â§13) uses DropCodes as its primary
//    input signal.  Ambiguous or missing codes corrupt the ML feedback loop.
//
// 2. OmegaError variants map 1-to-1 to architectural layers so that the Health
//    FSM (Â§3) can transition the correct layer to Degraded/Halted on receipt of
//    a typed error.
//
// 3. Simulation errors are sub-classified (Â§13.4, fix M3) â€” a single
//    LOST_SIMULATION_ERROR code was insufficient; the three sub-types have
//    distinct root causes and distinct corrective feedback actions.
//
// Spec references:
//   Â§3    â€” Health FSM: layer transitions triggered by OmegaError variants
//   Â§7    â€” Gas model: MissGas, MissGasSpike DropCodes
//   Â§8    â€” OFA compliance: MissOfaConsent, MissOfaSlippage, MissOfaOrder
//   Â§9    â€” DAG cycle detection: MissDagCycle
//   Â§11   â€” LA-specific drops: MissHfNotLiquidatable, MissFlashloan
//   Â§12   â€” Gas War: MissCompetition, MissCapacity
//   Â§13.4 â€” Simulation sub-classification: SimulationStateMismatch,
//            SimulationExecutionRevert, SimulationGasMiscalc
//   Â§16   â€” Observability: all DropCodes are always-sampled LA events

use std::fmt;
use thiserror::Error;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// DropCode
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Reason a blueprint was discarded without on-chain execution.
///
/// Every code corresponds to a distinct decision point in the pipeline.
/// The Loss Attribution Engine (Â§13) aggregates these into per-feature-key
/// loss rates that drive the online ML model.
///
/// ### Code taxonomy
///
/// | Prefix   | Meaning                                               |
/// |----------|-------------------------------------------------------|
/// | `Miss*`  | Opportunity did not meet a threshold â€” expected drop  |
/// | `Sim*`   | Blueprint failed at simulation stage (Â§13.4)          |
/// | `Wrong*` | Blueprint was structurally invalid / misrouted        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropCode {
    // â”€â”€ Routing / identity â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Blueprint submitted to a relay targeting the wrong chain ID.
    /// Triggers `DropCode::WrongChainId` metric + blueprint discard.
    WrongChain,

    /// Same as `WrongChain` but caught at the blueprint-construction
    /// stage (chain_id field doesn't match the active ChainId).
    WrongChainId,

    // â”€â”€ Timing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Blueprint's `expiry_block` has passed before submission.
    MissExpiry,

    // â”€â”€ Gas / fee model (Â§7) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Estimated gas cost exceeds the dynamic_min_profit threshold.
    MissGas,

    /// Gas spike detected between blueprint construction and submission;
    /// expected profit is no longer sufficient at the elevated fee.
    MissGasSpike,

    // â”€â”€ Access control / whitelist â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Strategy contract is not in the StrategyRegistry whitelist (Â§8).
    MissWhitelist,

    // â”€â”€ Profitability â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// `expected_profit_net` is below `dynamic_min_profit` at scoring.
    MissProfit,

    // â”€â”€ Oracle â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Oracle feed is unavailable or stale beyond the trust window.
    MissOracle,

    /// Two oracle sources diverge beyond the acceptable threshold;
    /// neither can be trusted as ground truth.
    MissOracleDiverge,

    // â”€â”€ Execution quality â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Projected slippage exceeds `slippage_bps` limit.
    MissSlippage,

    /// Insufficient on-chain liquidity to execute the swap(s) at the
    /// required size.
    MissLiquidity,

    /// DEX-specific liquidity check failed (pool depth, tick range, etc.).
    MissDexLiquidity,

    /// Price impact exceeds the `price_impact_bps` threshold.
    MissPriceImpact,

    // â”€â”€ Competition / relay â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Competition probability too high; expected EV is negative after
    /// accounting for the likelihood of being outbid (Â§19).
    MissCompetition,

    // â”€â”€ Capacity â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Hot-path execution capacity exhausted (Microtx lane full).
    MissCapacity,

    /// Normal-path execution capacity exhausted.
    MissCapacityNormal,

    // â”€â”€ Risk â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Risk layer (omega-risk) vetoed the blueprint.
    MissRisk,

    /// Flash crash detected â€” price move exceeds safety threshold;
    /// all execution halted until oracle stabilises.
    MissFlashCrash,

    // â”€â”€ DAG â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// DAG dependency resolution detected a cycle; blueprint cannot be
    /// scheduled (Â§9).
    MissDagCycle,

    // â”€â”€ Flashloan (Â§11) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Required flashloan liquidity is unavailable from any provider.
    MissFlashloan,

    // â”€â”€ LA-specific (Â§11) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Position health factor is above 1.0 at execution time â€” no longer
    /// liquidatable.  Common in competitive markets where another searcher
    /// landed first.
    MissHfNotLiquidatable,

    // â”€â”€ OFA compliance (Â§8) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Order flow agreement consent is missing for this user/protocol.
    MissOfaConsent,

    /// OFA slippage protection check failed.
    MissOfaSlippage,

    /// OFA order validation failed (malformed or expired order).
    MissOfaOrder,

    // â”€â”€ Simulation sub-classification (Â§13.4, fix M3) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
    /// Simulation passed but on-chain state differed from the revm cache
    /// snapshot â€” stale EIL double-buffer (Â§6).
    ///
    /// Corrective action: trigger immediate revm cache refresh; reduce
    /// revm trust window from 2 blocks to 1 block.
    SimulationStateMismatch,

    /// Simulation passed but on-chain execution reverted for non-state
    /// reasons â€” calldata bug.
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
        // Prometheus counters and ELK log payloads (Â§16).
        let s = match self {
            DropCode::WrongChain               => "WRONG_CHAIN",
            DropCode::WrongChainId             => "WRONG_CHAIN_ID",
            DropCode::MissExpiry               => "MISS_EXPIRY",
            DropCode::MissGas                  => "MISS_GAS",
            DropCode::MissGasSpike             => "MISS_GAS_SPIKE",
            DropCode::MissWhitelist            => "MISS_WHITELIST",
            DropCode::MissProfit               => "MISS_PROFIT",
            DropCode::MissOracle               => "MISS_ORACLE",
            DropCode::MissOracleDiverge        => "MISS_ORACLE_DIVERGE",
            DropCode::MissSlippage             => "MISS_SLIPPAGE",
            DropCode::MissLiquidity            => "MISS_LIQUIDITY",
            DropCode::MissDexLiquidity         => "MISS_DEX_LIQUIDITY",
            DropCode::MissPriceImpact          => "MISS_PRICE_IMPACT",
            DropCode::MissCompetition          => "MISS_COMPETITION",
            DropCode::MissCapacity             => "MISS_CAPACITY",
            DropCode::MissCapacityNormal       => "MISS_CAPACITY_NORMAL",
            DropCode::MissRisk                 => "MISS_RISK",
            DropCode::MissFlashCrash           => "MISS_FLASH_CRASH",
            DropCode::MissDagCycle             => "MISS_DAG_CYCLE",
            DropCode::MissFlashloan            => "MISS_FLASHLOAN",
            DropCode::MissHfNotLiquidatable    => "MISS_HF_NOT_LIQUIDATABLE",
            DropCode::MissOfaConsent           => "MISS_OFA_CONSENT",
            DropCode::MissOfaSlippage          => "MISS_OFA_SLIPPAGE",
            DropCode::MissOfaOrder             => "MISS_OFA_ORDER",
            DropCode::SimulationStateMismatch  => "SIMULATION_STATE_MISMATCH",
            DropCode::SimulationExecutionRevert=> "SIMULATION_EXECUTION_REVERT",
            DropCode::SimulationGasMiscalc     => "SIMULATION_GAS_MISCALC",
        };
        f.write_str(s)
    }
}

impl DropCode {
    /// Returns `true` for simulation-stage drops (Â§13.4).
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
    /// model as loss events â€” they represent correct engine behaviour.
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
    /// fault requiring immediate circuit-breaker action (Â§13.4).
    #[inline]
    pub fn is_critical(self) -> bool {
        matches!(self, DropCode::SimulationExecutionRevert)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OmegaError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Top-level error type for the Omega Engine.
///
/// Variants map to architectural layers so the Health FSM orchestrator
/// (Â§3) can transition the correct layer on error receipt:
///
/// | Variant          | Layer triggered   | FSM transition      |
/// |------------------|-------------------|---------------------|
/// | Dropped          | Strategy          | none (expected)     |
/// | IntegrityFail    | Security          | Halted              |
/// | Oracle           | ExternalData      | Degraded â†’ Halted   |
/// | Relay            | Relay             | Degraded â†’ Halted   |
/// | Zk               | Zk                | Degraded â†’ Halted   |
/// | Config           | SystemHealth      | Halted              |
/// | ChainMismatch    | Orchestrator      | none (blueprint drop)|
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OmegaError {
    /// Blueprint was intentionally discarded.  This is NOT a fault â€”
    /// it is a normal pipeline outcome.  The embedded `DropCode` is
    /// the signal consumed by the Loss Attribution Engine (Â§13).
    #[error("Blueprint dropped: {code}")]
    Dropped { code: DropCode },

    /// An integrity invariant was violated (signature mismatch, hash
    /// mismatch, StrategyRegistry rejection).  Triggers Security layer
    /// Halted (Â§3, Â§8).
    #[error("Integrity failure: {detail}")]
    IntegrityFail { detail: String },

    /// Oracle layer error â€” feed unavailable, stale, or diverged beyond
    /// acceptable bounds.  Triggers ExternalData Degraded (Â§3).
    #[error("Oracle error: {msg}")]
    Oracle { msg: String },

    /// Relay submission error â€” connection failure, rate-limit, or relay
    /// rejection.  Triggers Relay Degraded (Â§3).
    #[error("Relay error: {msg}")]
    Relay { msg: String },

    /// ZK proof generation or verification error.  Triggers Zk Degraded
    /// (Â§3, Â§15).
    #[error("ZK error: {msg}")]
    Zk { msg: String },

    /// Static configuration error detected at startup or hot-reload.
    /// Triggers SystemHealth Halted immediately â€” the engine cannot
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
    /// requiring circuit-breaker action (Â§13.4, Â§3).
    #[inline]
    pub fn is_critical(&self) -> bool {
        match self {
            OmegaError::Dropped { code }  => code.is_critical(),
            OmegaError::IntegrityFail { .. } => true,
            OmegaError::Config { .. }     => true,
            _ => false,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_code_display_screaming_snake() {
        assert_eq!(DropCode::SimulationStateMismatch.to_string(),  "SIMULATION_STATE_MISMATCH");
        assert_eq!(DropCode::SimulationExecutionRevert.to_string(),"SIMULATION_EXECUTION_REVERT");
        assert_eq!(DropCode::SimulationGasMiscalc.to_string(),     "SIMULATION_GAS_MISCALC");
        assert_eq!(DropCode::MissHfNotLiquidatable.to_string(),    "MISS_HF_NOT_LIQUIDATABLE");
        assert_eq!(DropCode::WrongChainId.to_string(),             "WRONG_CHAIN_ID");
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

        let e2 = OmegaError::Oracle { msg: "stale".into() };
        assert_eq!(e2.drop_code(), None);
    }

    #[test]
    fn omega_error_is_critical() {
        assert!(OmegaError::dropped(DropCode::SimulationExecutionRevert).is_critical());
        assert!(OmegaError::IntegrityFail { detail: "hash mismatch".into() }.is_critical());
        assert!(OmegaError::Config { msg: "bad toml".into() }.is_critical());
        assert!(!OmegaError::dropped(DropCode::MissProfit).is_critical());
        assert!(!OmegaError::Relay { msg: "timeout".into() }.is_critical());
    }
}