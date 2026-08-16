// crates/omega-hot-path/src/simulator.rs
//
// MicrotxSimulator â€” in-process revm execution for the hot path (Â§4).
//
// ## Spec Â§4 constraints
//
//   Target latency: <1ms per blueprint.
//   Simulator: revm (in-process, zero-copy).
//   Max gas: < 200,000 per blueprint.
//   Max reads: 8 per blueprint (enforced by the read budget from HotPathGate).
//
// ## Simulation model
//
//   The simulator does not call the actual revm EVM â€” omega-hot-path has
//   no dependency on the `revm` crate (which requires omega-strategies).
//   Instead it implements the SimResult contract that callers (the
//   orchestrator, loss attribution) depend on:
//
//     - `profit_net`: derived from `blueprint.expected_profit_net`.
//     - `gas_used`:   derived from `blueprint.l2_exec_gas_estimate`.
//     - `simulator`:  always "revm".
//     - `success`:    `true` unless the gas estimate or profit checks fail.
//
//   In the full engine the orchestrator holds an `Arc<RevmCacheManager>`
//   from omega-strategies and calls into it; the hot-path crate exposes
//   the interface contract only.
//
// ## ZK commitment
//
//   For blueprints that require a ZK proof (Â§15), the simulator returns
//   a `SimResult` flagged with `requires_zk_commitment = true`.  The
//   orchestrator then routes through the ZK layer before relay submission.
//   Hot-path blueprints (SA, LA hot-tier) use T1 ZK which operates inline
//   and does not block the <1ms budget significantly.
//
// ## Audit fix (this revision): oracle freshness, price sanity, and
// slippage protection were entirely absent from this lane
//
// Before this revision, `simulate()` had zero dependency on oracle data
// and never read `bp.slippage_bps` at all. A blueprint could go
// admission â†’ simulation â†’ success on this sub-millisecond lane with a
// stale oracle, a non-sane or wildly-diverged price, or a slippage
// tolerance the system never configured for its strategy â€” entirely
// independent of whether `omega-execution::ExecutionPipeline`'s 16-check
// pipeline (`omega_risk::checks::run_all_checks`) would have caught the
// identical condition, because the hot path never goes through that
// pipeline at all. That pipeline is Stage 2c of `ExecutionPipeline::
// execute`, which this crate has no relationship to; this <1ms lane is a
// structurally separate execution route.
//
// Fixed by giving `simulate()` a mandatory `&OracleSnapshot` parameter
// and running four checks before any success can be reported:
//   - `omega_risk::checks::oracle_freshness_check` â€” all three oracle
//     feeds stale â†’ reject (check 7's logic).
//   - `omega_risk::checks::oracle_hierarchy_check` â€” Chainlink and Pyth
//     both fresh but disagree beyond threshold â†’ reject (check 8's logic).
//   - `omega_risk::checks::oracle_price_sanity_check` â€” a relied-upon
//     price is non-finite/non-positive, or the fresh spot price has
//     diverged too far from a fresh TWAP â†’ reject (check 16's logic,
//     `DropCode::MissFlashCrash`).
//   - `omega_risk::checks::slippage_check` â€” `bp.slippage_bps` exceeds
//     the per-strategy configured maximum â†’ reject (check 9's logic).
//
// These call the SAME `pub` functions `omega_risk::checks` exposes for
// exactly this purpose (see that crate's module doc comment) rather than
// a second, hot-path-local reimplementation â€” one source of truth for
// each threshold, callable from both execution routes.
//
// This makes `oracle` a required, non-optional parameter deliberately:
// there is no safe default for "I don't have live oracle data" other
// than failing every one of the checks above, which passing a
// synthetic/empty snapshot would not reliably do (e.g. a
// default-initialized snapshot might read as "fresh" with zero ages).
// The caller â€” whoever constructs a `HotPathRequest` â€” must assemble a
// live snapshot at request time, the same requirement
// `omega-execution::ExecutionPipeline::execute` already places on its
// `CheckContext` parameter.
//
// Placement: these four checks run after `Expired` (cheap, no
// allocation, already established the blueprint is still live) and
// before the read-budget/profit calculation that follows â€” rejecting
// stale/unsafe market data before doing any further work on it.
//
// Not touched by this fix, left exactly as before: `metrics.record_miss()`
// is NOT called inside `simulate()` for the new checks, matching the
// existing pattern for `WrongSimulator`/`GasLimitExceeded`/
// `ReadBudgetExhausted` (which also don't call it here) â€” `HotPathRunner::
// run` calls `metrics.record_miss()` exactly once for every `Err` variant
// in its match. (`Expired` and `Unprofitable` DO call it here as well as
// in `lib.rs`, a pre-existing double-count inconsistency in this file
// unrelated to oracle/price/slippage â€” left alone as out of scope for
// this change.)
//
// ## Audit fix (this revision, 2): test helpers missing flashloan
// provider/token + max_base_fee_gwei fields
//
// `omega-core` added four more required fields to `ExecutionBlueprint`
// (`flashloan_provider_type`, `provider_contract`, `flashloan_token`,
// `max_base_fee_gwei`). `MicrotxSimulator::simulate` reads none of them â€”
// this crate's own flashloan feasibility handling (if any) lives
// elsewhere in the pipeline, not on this <1ms lane â€” so this is a
// test-construction-only fix, same category as the oracle/slippage test
// helper additions already in this file.

use std::time::Instant;

use alloy_primitives::U256;
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
// Simulator is defined in omega_core::types::lane, not blueprint.
// blueprint re-exports it via `use` but does not make it pub from that path.
use omega_core::types::lane::Simulator;
use omega_core::types::strategy::SimResult;
use omega_risk::checks::{
    oracle_freshness_check, oracle_hierarchy_check, oracle_price_sanity_check, slippage_check,
};
use omega_risk::context::{
    OracleSnapshot, MAX_SLIPPAGE_BPS_LA, MAX_SLIPPAGE_BPS_MEV, MAX_SLIPPAGE_BPS_MSA,
    MAX_SLIPPAGE_BPS_SA,
};

use crate::gate::MICROTX_GAS_LIMIT;
use crate::metrics::HotPathMetrics;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SimulationError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// Blueprint's simulator field is not revm â€” wrong path.
    #[error("Hot-path simulator requires Simulator::Revm; got {actual:?}")]
    WrongSimulator { actual: Simulator },

    /// Gas estimate exceeds the Microtx limit.
    #[error("Gas estimate {gas} â‰¥ MICROTX_GAS_LIMIT {limit}")]
    GasLimitExceeded { gas: u64, limit: u64 },

    /// Blueprint already expired at the current block.
    #[error("Blueprint expired at block {expiry}; current block {current}")]
    Expired { expiry: u64, current: u64 },

    /// All oracle feeds are stale â€” see
    /// `omega_risk::checks::oracle_freshness_check`.
    #[error("Oracle data is stale: no fresh Chainlink, Pyth, or TWAP feed")]
    OracleStale,

    /// Chainlink and Pyth are both fresh but diverge beyond the
    /// configured threshold â€” see
    /// `omega_risk::checks::oracle_hierarchy_check`.
    #[error("Oracle feeds diverge beyond threshold: Chainlink and Pyth disagree")]
    OracleDiverged,

    /// An active oracle price is non-sane (non-finite or non-positive),
    /// or the fresh spot price has diverged too far from a fresh TWAP â€”
    /// see `omega_risk::checks::oracle_price_sanity_check`.
    #[error("Oracle price sanity check failed: non-sane price or spot/TWAP divergence")]
    PriceSanityViolation,

    /// The blueprint's requested slippage tolerance exceeds the
    /// configured maximum for its strategy â€” see
    /// `omega_risk::checks::slippage_check`.
    #[error("Slippage {slippage_bps} bps exceeds strategy max {max_bps} bps")]
    SlippageExceeded { slippage_bps: u16, max_bps: u16 },

    /// Simulation produced zero or negative profit (after gas deduction).
    #[error("Simulation produced unprofitable result: profit_net={profit_net}")]
    Unprofitable { profit_net: U256 },

    /// Read budget exhausted â€” callee tried to make more than 8 RPC reads.
    #[error("Read budget exhausted: {used} reads > {budget} budget")]
    ReadBudgetExhausted { used: u8, budget: u8 },
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// HotPathSimResult
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Extended simulation result for hot-path blueprints.
#[derive(Debug, Clone)]
pub struct HotPathSimResult {
    /// Core simulation result for loss attribution and relay submission.
    pub inner: SimResult,
    /// Wall-clock latency of the simulation in microseconds.
    pub latency_us: u64,
    /// Number of RPC reads consumed during simulation.
    pub reads_used: u8,
    /// Whether this blueprint requires a ZK commitment before relay (Â§15).
    pub requires_zk_commitment: bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Per-strategy slippage cap selection
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Selects the configured slippage cap for a blueprint's strategy.
///
/// `HotPathGate::admit` only ever lets `StrategyId::Sa` and
/// `StrategyId::La` reach `simulate()` (Â§4 â€” canary, MSA, and MEV are all
/// rejected at admission). The `Cnry`/`Msa`/`Mev` arm below exists purely
/// as defense-in-depth so this function can never be asked to return "no
/// limit" â€” if `simulate()` is ever called directly for one of those
/// strategies (bypassing the gate â€” e.g. a future caller, or a test),
/// this applies the SMALLEST of all four configured per-strategy caps
/// rather than guessing which single one was intended.
fn strategy_max_slippage_bps(id: StrategyId) -> u16 {
    match id {
        StrategyId::Sa => MAX_SLIPPAGE_BPS_SA,
        StrategyId::La => MAX_SLIPPAGE_BPS_LA,
        StrategyId::Cnry | StrategyId::Msa | StrategyId::Mev => MAX_SLIPPAGE_BPS_SA
            .min(MAX_SLIPPAGE_BPS_LA)
            .min(MAX_SLIPPAGE_BPS_MSA)
            .min(MAX_SLIPPAGE_BPS_MEV),
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// MicrotxSimulator
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// In-process simulation executor for the Microtx hot path.
#[derive(Debug, Clone)]
pub struct MicrotxSimulator {
    current_block: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MicrotxSimulator {
    pub fn new(initial_block: u64) -> Self {
        Self {
            current_block: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(initial_block)),
        }
    }

    pub fn set_block(&self, block: u64) {
        self.current_block
            .store(block, std::sync::atomic::Ordering::Release);
    }

    /// Simulate a Microtx blueprint.
    ///
    /// `oracle` must be a live snapshot assembled by the caller at
    /// request time â€” see this file's module-level audit note for why
    /// there is no safe default and why the four oracle/slippage checks
    /// below cannot be skipped.
    pub fn simulate(
        &self,
        bp: &ExecutionBlueprint,
        read_budget: u8,
        oracle: &OracleSnapshot,
        metrics: &HotPathMetrics,
    ) -> Result<HotPathSimResult, SimulationError> {
        let t0 = Instant::now();

        if bp.simulator != Simulator::Revm {
            return Err(SimulationError::WrongSimulator {
                actual: bp.simulator,
            });
        }

        if bp.l2_exec_gas_estimate >= MICROTX_GAS_LIMIT {
            return Err(SimulationError::GasLimitExceeded {
                gas: bp.l2_exec_gas_estimate,
                limit: MICROTX_GAS_LIMIT,
            });
        }

        let current = self
            .current_block
            .load(std::sync::atomic::Ordering::Acquire);
        if bp.is_expired(current) {
            metrics.record_miss();
            return Err(SimulationError::Expired {
                expiry: bp.expiry_block,
                current,
            });
        }

        // Oracle freshness / hierarchy / price sanity / slippage â€” see
        // this file's module-level audit note for why these four checks
        // exist here at all and why they must run unconditionally before
        // any success path.
        if oracle_freshness_check(oracle).is_some() {
            return Err(SimulationError::OracleStale);
        }
        if oracle_hierarchy_check(oracle).is_some() {
            return Err(SimulationError::OracleDiverged);
        }
        if oracle_price_sanity_check(oracle).is_some() {
            return Err(SimulationError::PriceSanityViolation);
        }
        let max_slippage_bps = strategy_max_slippage_bps(bp.strategy_id);
        if slippage_check(bp.slippage_bps, max_slippage_bps).is_some() {
            return Err(SimulationError::SlippageExceeded {
                slippage_bps: bp.slippage_bps,
                max_bps: max_slippage_bps,
            });
        }

        let reads_used: u8 = self.estimate_reads(bp).min(read_budget);

        if reads_used > read_budget {
            return Err(SimulationError::ReadBudgetExhausted {
                used: reads_used,
                budget: read_budget,
            });
        }

        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.90) as u64;
        let l2_cost_wei = gas_used as u128 * bp.base_fee_at_creation as u128 * 1_000_000_000;

        let profit_net = if bp.expected_profit_net > U256::from(l2_cost_wei) {
            bp.expected_profit_net - U256::from(l2_cost_wei)
        } else {
            U256::ZERO
        };

        if profit_net == U256::ZERO {
            metrics.record_miss();
            return Err(SimulationError::Unprofitable { profit_net });
        }

        let latency_us = t0.elapsed().as_micros() as u64;

        let result = HotPathSimResult {
            inner: SimResult {
                profit_net,
                gas_used,
                simulator: "revm".to_string(),
                success: true,
            },
            latency_us,
            reads_used,
            requires_zk_commitment: bp.zk_proof_commitment.is_some(),
        };

        metrics.record_success(latency_us, profit_net);

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            latency_us,
            gas_used,
            profit_net     = %profit_net,
            reads_used,
            "MicrotxSimulator: simulation complete",
        );

        Ok(result)
    }

    fn estimate_reads(&self, bp: &ExecutionBlueprint) -> u8 {
        let fraction = bp.l2_exec_gas_estimate as f64 / MICROTX_GAS_LIMIT as f64;
        (fraction * 8.0).ceil() as u8
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

// See gate.rs's test module for why this scoped allow is needed: this
// crate's Cargo.toml `[lints]` table sets clippy::unwrap_used/expect_used
// to "warn" unconditionally (no cfg(test) carve-out possible at the
// manifest level), and `cargo clippy -- -D warnings` promotes that to a
// hard error for this module's ordinary test-only `.unwrap()`/
// `.unwrap_err()` calls otherwise.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, Bytes, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::flashloan_provider::FlashloanProviderType;
    use omega_core::types::lane::Lane;
    use uuid::Uuid;

    use crate::metrics::HotPathMetrics;

    fn make_bp(gas: u64, profit: u128, expiry: u64, sim: Simulator) -> ExecutionBlueprint {
        make_bp_with_slippage(gas, profit, expiry, sim, StrategyId::Sa, 20)
    }

    /// Full constructor allowing strategy and slippage to vary, needed by
    /// the new slippage tests below. `slippage_bps` defaults to 20 in
    /// `make_bp` â€” comfortably under `MAX_SLIPPAGE_BPS_SA` (30) â€” since a
    /// hardcoded 100 (the value this file previously used before slippage
    /// was actually enforced) would now fail every test's slippage check.
    ///
    /// `flashloan_provider_type`/`provider_contract`/`flashloan_token`/
    /// `max_base_fee_gwei`: none of these blueprints source a real
    /// flashloan and `MicrotxSimulator::simulate` reads none of the four
    /// â€” see this file's audit note â€” so these are ZERO/placeholder
    /// values, same treatment as `idempotency_key` below.
    fn make_bp_with_slippage(
        gas: u64,
        profit: u128,
        expiry: u64,
        sim: Simulator,
        strategy_id: StrategyId,
        slippage_bps: u16,
    ) -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([3u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(strategy_id, 42161, 0, signal_id);
        ExecutionBlueprint {
            blueprint_hash: B256::from([2u8; 32]),
            chain_id: 42161,
            strategy_id,
            lane: Lane::Microtx,
            simulator: sim,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            signal_id,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            calldata: Bytes::default(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: gas,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(profit),
            dynamic_min_profit: U256::from(100_000_u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 10,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(10, 3.0),
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: expiry,
            nonce: 0,
            confirmation_depth: 12,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["relay_a".into()],
            zk_proof_commitment: None,
        }
    }

    fn sim() -> MicrotxSimulator {
        MicrotxSimulator::new(1_000_000)
    }
    fn metrics() -> HotPathMetrics {
        HotPathMetrics::new()
    }

    /// A live, sane, mutually-consistent oracle snapshot â€” every check
    /// this file adds should pass against this.
    fn passing_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0, // ~0.05% divergence from Chainlink â€” within threshold
            twap_price: 1999.0, // ~0.05% divergence from spot â€” well within flash-crash threshold
            chainlink_age_s: 10,
            pyth_age_s: 10,
            twap_age_s: 60,
        }
    }

    /// All three feeds stale â€” must fail `oracle_freshness_check`.
    fn stale_oracle() -> OracleSnapshot {
        OracleSnapshot {
            chainlink_price: 2000.0,
            pyth_price: 2001.0,
            twap_price: 1999.0,
            chainlink_age_s: 100, // > 45s
            pyth_age_s: 100,      // > 45s
            twap_age_s: 200,      // > 120s
        }
    }

    #[test]
    fn successful_simulation_returns_revm_simulator() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let r = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap();
        assert_eq!(r.inner.simulator, "revm");
        assert!(r.inner.success);
        assert!(r.inner.gas_used > 0 && r.inner.gas_used < 100_000);
    }

    #[test]
    fn latency_is_recorded() {
        let bp = make_bp(
            50_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let r = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap();
        assert!(r.latency_us < 100_000);
    }

    #[test]
    fn wrong_simulator_returns_error() {
        let bp = make_bp(100_000, 10_000_000, 2_000_000, Simulator::Anvil);
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::WrongSimulator { .. }));
    }

    #[test]
    fn expired_blueprint_returns_error() {
        let s = MicrotxSimulator::new(2_000_001);
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let e = s
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(e, SimulationError::Expired { .. }));
    }

    #[test]
    fn gas_over_limit_returns_error() {
        let bp = make_bp(MICROTX_GAS_LIMIT, 10_000_000, 2_000_000, Simulator::Revm);
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::GasLimitExceeded { .. }));
    }

    #[test]
    fn set_block_updates_expiry_check() {
        let s = sim();
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        assert!(s.simulate(&bp, 8, &passing_oracle(), &metrics()).is_ok());
        s.set_block(3_000_000);
        assert!(matches!(
            s.simulate(&bp, 8, &passing_oracle(), &metrics()),
            Err(SimulationError::Expired { .. })
        ));
    }

    // â”€â”€ Oracle freshness â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn stale_oracle_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let err = sim()
            .simulate(&bp, 8, &stale_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(err, SimulationError::OracleStale));
    }

    #[test]
    fn only_twap_fresh_is_not_stale_but_has_no_divergence_to_check() {
        // All three feeds fresh->stale combinations are exercised in
        // omega-risk's own test suite; this confirms the hot path reaches
        // the SAME conclusion for the identical snapshot shape (only TWAP
        // fresh), rather than diverging in behavior between the two
        // execution routes.
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_age_s = 100;
        oracle.pyth_age_s = 100;
        // twap_age_s stays fresh (60s)
        assert!(sim().simulate(&bp, 8, &oracle, &metrics()).is_ok());
    }

    // â”€â”€ Oracle hierarchy (Chainlink vs Pyth divergence) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn chainlink_pyth_divergence_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 2000.0;
        oracle.pyth_price = 2010.0; // 0.5% > 0.4% threshold
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::OracleDiverged));
    }

    // â”€â”€ Price sanity / flash-crash guard â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn non_positive_price_on_fresh_feed_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 0.0;
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    #[test]
    fn nan_price_on_fresh_feed_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.twap_price = f64::NAN;
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    #[test]
    fn spot_twap_flash_crash_divergence_is_rejected() {
        let bp = make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        );
        let mut oracle = passing_oracle();
        oracle.chainlink_price = 2000.0;
        oracle.pyth_price = 2000.0; // agree with chainlink, so check 8 passes
        oracle.twap_price = 1000.0; // 100% divergence from spot
        let err = sim().simulate(&bp, 8, &oracle, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::PriceSanityViolation));
    }

    // â”€â”€ Slippage â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn slippage_over_strategy_max_is_rejected() {
        // SA's cap is MAX_SLIPPAGE_BPS_SA (30) â€” 50 exceeds it.
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::Sa,
            50,
        );
        let err = sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .unwrap_err();
        assert!(matches!(
            err,
            SimulationError::SlippageExceeded {
                slippage_bps: 50,
                max_bps: MAX_SLIPPAGE_BPS_SA
            }
        ));
    }

    #[test]
    fn slippage_exactly_at_strategy_max_passes() {
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::Sa,
            MAX_SLIPPAGE_BPS_SA,
        );
        assert!(sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .is_ok());
    }

    #[test]
    fn la_strategy_uses_la_slippage_cap_not_sa() {
        // LA's cap (MAX_SLIPPAGE_BPS_LA = 100) is looser than SA's (30) â€”
        // a slippage_bps of 60 must pass for LA even though it would fail
        // for SA, proving the strategy-specific cap is actually selected
        // and not hardcoded to SA's.
        let bp = make_bp_with_slippage(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
            StrategyId::La,
            60,
        );
        assert!(sim()
            .simulate(&bp, 8, &passing_oracle(), &metrics())
            .is_ok());
    }

    // â”€â”€ Concurrency: cannot be bypassed under concurrent execution â”€â”€â”€â”€â”€â”€

    #[test]
    fn concurrent_calls_with_stale_oracle_all_rejected() {
        let s = std::sync::Arc::new(sim());
        let bp = std::sync::Arc::new(make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        ));
        let oracle = std::sync::Arc::new(stale_oracle());

        let mut handles = Vec::new();
        for _ in 0..50 {
            let s = std::sync::Arc::clone(&s);
            let bp = std::sync::Arc::clone(&bp);
            let oracle = std::sync::Arc::clone(&oracle);
            handles.push(std::thread::spawn(move || {
                matches!(
                    s.simulate(&bp, 8, &oracle, &metrics()),
                    Err(SimulationError::OracleStale)
                )
            }));
        }

        let mut all_rejected = true;
        for h in handles {
            all_rejected &= h.join().expect("test thread must not panic");
        }
        assert!(
            all_rejected,
            "every concurrent call against a stale oracle must be rejected â€” none may slip through"
        );
    }

    #[test]
    fn concurrent_calls_with_sane_oracle_all_succeed() {
        // Control test for the one above: proves the concurrency
        // harness itself isn't what's causing rejections â€” a sane,
        // fresh, mutually-consistent oracle snapshot must let every
        // concurrent call through.
        let s = std::sync::Arc::new(sim());
        let bp = std::sync::Arc::new(make_bp(
            100_000,
            2_000_000_000_000_000_u128,
            2_000_000,
            Simulator::Revm,
        ));
        let oracle = std::sync::Arc::new(passing_oracle());

        let mut handles = Vec::new();
        for _ in 0..50 {
            let s = std::sync::Arc::clone(&s);
            let bp = std::sync::Arc::clone(&bp);
            let oracle = std::sync::Arc::clone(&oracle);
            handles.push(std::thread::spawn(move || {
                s.simulate(&bp, 8, &oracle, &metrics()).is_ok()
            }));
        }

        let mut all_ok = true;
        for h in handles {
            all_ok &= h.join().expect("test thread must not panic");
        }
        assert!(
            all_ok,
            "every concurrent call against a sane oracle must succeed"
        );
    }
}
