// crates/omega-core/src/types/strategy.rs
//
// Core strategy abstraction — the trait every Omega strategy implements.
//
// ## Audit findings fixed in this pass
//
// `OpScore` and `SimResult` both had documented invariants the type
// system didn't enforce:
//   - OpScore: "score of exactly 0.0 is always skipped" is checked by
//     `should_proceed`, but nothing confirmed `score`/`competition_prob`
//     were even finite, non-NaN values in the first place. (NaN
//     comparisons are always false in IEEE-754, so `should_proceed`
//     already fails safe on a NaN score without any change — verified
//     and pinned with a test rather than "fixed," since it was already
//     correct — but Infinity passes `> 0.0` and was previously
//     undetectable.) Added `is_well_formed()` as an explicit sanity
//     check independent of the business-logic gate.
//   - SimResult: "false [success] → the profit figures are invalid" was
//     documented but nothing stopped a caller from reading `profit_net`
//     directly without checking `success` first. Added
//     `profit_net_if_successful()` as the safe accessor.
//
// Neither fix restructures the types (e.g. into tagged enums) or
// changes any existing field — both are purely additive, since
// `StrategyTrait` implementations (which construct/consume these types)
// live in omega-strategies and aren't visible from here; a breaking
// restructure would risk guessing at call sites this crate can't see.
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
///
/// NOTE: this doc comment describes a CONTRACT between whatever
/// `StrategyTrait::score` implementation produces an `OpScore` and
/// whatever consumes it — specifically, that `score` already
/// incorporates `competition_prob`. Nothing in this generic type can
/// verify that semantic coupling (it's strategy-specific business
/// logic); `is_well_formed()` below only checks structural sanity
/// (finite, non-NaN, in-range), not that coupling.
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
    ///
    /// NaN handling: `self.score > 0.0` is `false` for a NaN score
    /// under IEEE-754 comparison semantics, so a NaN score already
    /// fails safe here without any special-case code — verified by
    /// `nan_score_does_not_proceed` below. This was already correct;
    /// it just wasn't documented or tested before.
    #[inline]
    pub fn should_proceed(&self) -> bool {
        self.score > 0.0 && self.expected_profit > U256::ZERO
    }

    /// Structural sanity check independent of `should_proceed`'s
    /// business-logic gate: confirms `score` and `competition_prob` are
    /// both finite, non-NaN, and within their documented [0.0, 1.0]
    /// range. `should_proceed()` already fails safe on NaN (see its doc
    /// comment) but silently PASSES an infinite score (`f64::INFINITY >
    /// 0.0` is `true`) — this catches that case explicitly. Does NOT
    /// verify the `score` ⊇ `competition_prob` semantic coupling
    /// documented on the struct — that can only be checked by the
    /// strategy implementation that constructed this value. Call this
    /// before trusting an `OpScore` from an untrusted or external
    /// source (e.g. deserialized from a wire payload).
    pub fn is_well_formed(&self) -> bool {
        self.score.is_finite()
            && (0.0..=1.0).contains(&self.score)
            && self.competition_prob.is_finite()
            && (0.0..=1.0).contains(&self.competition_prob)
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

impl SimResult {
    /// Returns `profit_net` only when the simulation actually
    /// succeeded, `None` otherwise.
    ///
    /// `profit_net` is documented as invalid when `success` is `false`,
    /// but nothing in the type system stops a caller from reading the
    /// raw field directly and skipping that check. This is the safe
    /// accessor that makes the invariant impossible to miss.
    #[inline]
    pub fn profit_net_if_successful(&self) -> Option<U256> {
        self.success.then_some(self.profit_net)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_proceed_requires_positive_score_and_profit() {
        let score = OpScore { score: 0.5, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(score.should_proceed());

        let zero_score = OpScore { score: 0.0, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(!zero_score.should_proceed());

        let zero_profit = OpScore { score: 0.5, expected_profit: U256::ZERO, competition_prob: 0.2 };
        assert!(!zero_profit.should_proceed());
    }

    #[test]
    fn nan_score_does_not_proceed() {
        // Confirms should_proceed() already fails safe on NaN via
        // IEEE-754 comparison semantics (NaN > 0.0 is false) — pinning
        // this behavior with a test so it can't be "fixed" into a bug
        // later by someone unaware NaN comparisons are intentionally
        // false here.
        let score = OpScore {
            score: f64::NAN,
            expected_profit: U256::from(1u64),
            competition_prob: 0.2,
        };
        assert!(!score.should_proceed());
    }

    #[test]
    fn is_well_formed_rejects_nan_and_infinite() {
        let nan_score = OpScore { score: f64::NAN, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(!nan_score.is_well_formed());

        let inf_score = OpScore { score: f64::INFINITY, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(!inf_score.is_well_formed(), "should_proceed() would wrongly accept this; is_well_formed() must reject it");

        let inf_competition = OpScore { score: 0.5, expected_profit: U256::from(1u64), competition_prob: f64::INFINITY };
        assert!(!inf_competition.is_well_formed());
    }

    #[test]
    fn is_well_formed_rejects_out_of_range() {
        let over = OpScore { score: 1.5, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(!over.is_well_formed());

        let negative = OpScore { score: -0.1, expected_profit: U256::from(1u64), competition_prob: 0.2 };
        assert!(!negative.is_well_formed());
    }

    #[test]
    fn is_well_formed_accepts_valid_score() {
        let ok = OpScore { score: 0.75, expected_profit: U256::from(1u64), competition_prob: 0.3 };
        assert!(ok.is_well_formed());
    }

    #[test]
    fn profit_net_if_successful_returns_none_on_failure() {
        let result = SimResult {
            profit_net: U256::from(1_000_000u64), // documented-invalid leftover value
            gas_used: 50_000,
            simulator: "revm".to_string(),
            success: false,
        };
        assert_eq!(
            result.profit_net_if_successful(),
            None,
            "profit_net must not be trusted when success is false"
        );
    }

    #[test]
    fn profit_net_if_successful_returns_value_on_success() {
        let result = SimResult {
            profit_net: U256::from(1_000_000u64),
            gas_used: 50_000,
            simulator: "revm".to_string(),
            success: true,
        };
        assert_eq!(result.profit_net_if_successful(), Some(U256::from(1_000_000u64)));
    }
}