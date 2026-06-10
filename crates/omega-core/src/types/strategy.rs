// crates/omega-core/src/types/strategy.rs
//
// Core strategy abstraction — the trait every Omega strategy implements.
//
// Spec references:
//   §1.1  — strategy phases and StrategyId
//   §4    — simulation backend selection (Lane / Simulator)
//   §7    — Arbitrum dual-component gas model (feeds into OpScore)
//   §11   — LA hot-path requirements (hot_path_eligible, gas_budget)
//   §19   — EV-weighted rollout (score → OpScore)
//   §20   — phase gates (phase_required enforced at activation)
//
// Architecture note (§22.1):
//   StrategyTrait lives in omega-core so every crate in the dependency
//   graph can hold `Arc<dyn StrategyTrait>` without depending on
//   omega-strategies.  Concrete implementations live in omega-strategies.

use alloy_primitives::{Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::blueprint::{ExecutionBlueprint, StrategyId};
use crate::types::lane::Lane;

// ─────────────────────────────────────────────────────────────────────────────
// SignalState
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot of oracle / market state passed to every strategy scorer.
///
/// Produced by the oracle layer (omega-oracle) and stamped with a
/// monotonically increasing `state_version`.  Strategies record
/// `signal_state_hash` and `state_version` on the blueprint they
/// construct so that the EIL double-buffer can detect staleness before
/// simulation (§6, §13.4 SIMULATION_STATE_MISMATCH).
///
/// This struct is deliberately thin — it carries only the fields needed
/// by the trait interface.  Strategy-specific oracle data (pool reserves,
/// position health factors, etc.) is fetched by each concrete
/// implementation via its injected oracle client.
#[derive(Debug, Clone)]
pub struct SignalState {
    /// Monotonically increasing snapshot version from the EIL (§6).
    pub state_version: u64,

    /// EIP-155 chain ID.
    pub chain_id: u64,

    /// Latest confirmed block number at snapshot time.
    pub block_number: u64,

    /// EIP-1559 base fee in gwei at snapshot time.
    /// Fed into the dual-component gas model (§7).
    pub base_fee_gwei: u64,

    /// L1 data fee oracle reading in gwei at snapshot time.
    /// Arbitrum-specific — see §7, §12.2.
    pub l1_data_fee_gwei: u64,

    /// keccak256 of the full oracle state at this snapshot.
    /// Stored on every blueprint as `signal_state_hash`.
    pub state_hash: alloy_primitives::B256,
}

// ─────────────────────────────────────────────────────────────────────────────
// OpScore
// ─────────────────────────────────────────────────────────────────────────────

/// Opportunity score returned by [`StrategyTrait::score`].
///
/// Used by the DAG dispatcher (§9) and the EV-weighted rollout (§19)
/// to rank competing opportunities before blueprint construction.
///
/// ## Score semantics
///
/// `score` is a dimensionless float in [0.0, 1.0] representing the
/// adjusted expected value of the opportunity after accounting for
/// competition probability and gas cost.  The exact formula is strategy-
/// specific but MUST incorporate:
///   - `expected_profit` (net of gas and flashloan premium)
///   - `competition_prob` (estimated probability of being outbid)
///   - current `dynamic_min_profit` threshold
///
/// A score of 0.0 means the opportunity does not meet the minimum
/// threshold and MUST NOT proceed to `build_blueprint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpScore {
    /// Dimensionless score in [0.0, 1.0].  Zero → skip.
    pub score: f64,

    /// Expected net profit in wei after all costs.
    pub expected_profit: U256,

    /// Estimated probability (0.0–1.0) that a competitor will win the
    /// same opportunity.  Higher values reduce the effective score.
    pub competition_prob: f64,
}

impl OpScore {
    /// Returns `true` when this opportunity should proceed to blueprint
    /// construction.  A score of exactly 0.0 is always skipped.
    #[inline]
    pub fn should_proceed(&self) -> bool {
        self.score > 0.0 && self.expected_profit > U256::ZERO
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SimResult
// ─────────────────────────────────────────────────────────────────────────────

/// Result of simulating an [`ExecutionBlueprint`].
///
/// Returned by [`StrategyTrait::simulate`].  The Loss Attribution
/// Engine (§13) compares `profit_net` against the blueprint's
/// `expected_profit_net` to detect SIMULATION_GAS_MISCALC events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimResult {
    /// Simulated net profit in wei.
    pub profit_net: U256,

    /// Actual gas units consumed in simulation.
    /// Compared against `l2_exec_gas_estimate` for gas model calibration.
    pub gas_used: u64,

    /// Name of the simulator backend that produced this result
    /// ("revm" or "anvil").  Must match the `simulator` field on the
    /// blueprint that was simulated.
    pub simulator: String,

    /// Whether the simulation succeeded without revert.
    /// `false` → the profit figures are invalid; this is a
    /// SIMULATION_EXECUTION_REVERT (§13.4).
    pub success: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// StrategyTrait
// ─────────────────────────────────────────────────────────────────────────────

/// Core strategy interface — every Omega strategy implements this trait.
///
/// Implementations live in omega-strategies; the trait lives in
/// omega-core so the rest of the system can hold `Arc<dyn StrategyTrait>`
/// without depending on omega-strategies (§22.1).
///
/// ## Execution flow
///
/// ```text
/// oracle tick
///   → score(signal)          [cheap, synchronous-ish, no blueprint alloc]
///   → build_blueprint(signal) [allocates blueprint, fetches calldata]
///   → simulate(blueprint)     [revm or Anvil fork]
///   → relay submission        [Gas War Engine, §12]
/// ```
///
/// Canary strategies override only `score` and `is_canary` — they never
/// reach `build_blueprint` or `simulate` in production.
#[async_trait]
pub trait StrategyTrait: Send + Sync {
    // ── Static metadata ───────────────────────────────────────────────────

    /// Strategy discriminant.  Determines slot priority and phase gate.
    fn strategy_id(&self) -> StrategyId;

    /// Slot competition priority (mirrors `StrategyId::priority`).
    fn priority(&self) -> u8 {
        self.strategy_id().priority()
    }

    /// Execution lane this strategy targets.
    fn lane(&self) -> Lane;

    /// Whether this strategy is eligible for the <1ms hot-path (§11.1).
    /// True only for SA (Microtx lane) and LA hot tier (HF < 1.01).
    fn hot_path_eligible(&self) -> bool;

    /// Maximum gas budget (L2 units) the strategy may consume.
    /// Enforced by the Gas War Engine before submission (§12).
    fn gas_budget(&self) -> u64;

    /// Base minimum profit threshold in wei, before dynamic adjustment
    /// (§7).  The dynamic_min_profit on a blueprint may be higher after
    /// accounting for current fee conditions.
    fn base_min_profit_wei(&self) -> U256;

    /// keccak256 of the expected deployed strategy contract bytecode.
    /// Validated by the StrategyRegistry (§8) before simulation.
    fn expected_bytecode_hash(&self) -> B256;

    /// Returns `true` for the Canary strategy (§1.1).
    ///
    /// Default implementation delegates to `strategy_id().is_canary()`.
    fn is_canary(&self) -> bool {
        self.strategy_id().is_canary()
    }

    // ── Execution pipeline ────────────────────────────────────────────────

    /// Score the current opportunity.
    ///
    /// Called on every relevant oracle tick.  MUST be fast — this is
    /// in the hot evaluation path.  Should avoid network I/O where
    /// possible; use cached oracle state.
    ///
    /// Returns `OpScore::score == 0.0` when the opportunity is below
    /// threshold.  The DAG stops the pipeline without proceeding to
    /// `build_blueprint`.
    async fn score(&self, signal: &SignalState) -> Result<OpScore>;

    /// Build a fully-specified [`ExecutionBlueprint`] for the given
    /// signal state.
    ///
    /// Only called when `score` returns `should_proceed() == true`.
    /// Implementations must:
    ///   1. Fetch any additional oracle data needed for calldata encoding.
    ///   2. Apply the dual-component gas model (§7).
    ///   3. Compute `blueprint_hash` over all other fields.
    ///   4. Set `simulator` via `ExecutionBlueprint::select_simulator`.
    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint>;

    /// Simulate the blueprint and return the result.
    ///
    /// Implementation must dispatch to revm or Anvil based on
    /// `blueprint.simulator`.  The returned `SimResult::success` flag
    /// determines whether Loss Attribution records a
    /// SIMULATION_EXECUTION_REVERT (§13.4).
    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult>;

    /// Encode the final calldata for relay submission.
    ///
    /// Called after simulation succeeds.  Returns ABI-encoded call to
    /// the strategy contract.  For most strategies this is identical to
    /// `blueprint.calldata`; some strategies re-encode with final
    /// slippage parameters computed from the simulation result.
    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes;
}
