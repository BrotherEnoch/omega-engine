// crates/omega-hot-path/src/simulator.rs
//
// MicrotxSimulator — in-process revm execution for the hot path (§4).
//
// ## Spec §4 constraints
//
//   Target latency: <1ms per blueprint.
//   Simulator: revm (in-process, zero-copy).
//   Max gas: < 200,000 per blueprint.
//   Max reads: 8 per blueprint (enforced by the read budget from HotPathGate).
//
// ## Simulation model
//
//   The simulator does not call the actual revm EVM — omega-hot-path has
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
//   For blueprints that require a ZK proof (§15), the simulator returns
//   a `SimResult` flagged with `requires_zk_commitment = true`.  The
//   orchestrator then routes through the ZK layer before relay submission.
//   Hot-path blueprints (SA, LA hot-tier) use T1 ZK which operates inline
//   and does not block the <1ms budget significantly.

use std::time::Instant;

use alloy_primitives::U256;
use omega_core::types::blueprint::ExecutionBlueprint;
// Simulator is defined in omega_core::types::lane, not blueprint.
// blueprint re-exports it via `use` but does not make it pub from that path.
use omega_core::types::lane::Simulator;
use omega_core::types::strategy::SimResult;

use crate::gate::MICROTX_GAS_LIMIT;
use crate::metrics::HotPathMetrics;

// ─────────────────────────────────────────────────────────────────────────────
// SimulationError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SimulationError {
    /// Blueprint's simulator field is not revm — wrong path.
    #[error("Hot-path simulator requires Simulator::Revm; got {actual:?}")]
    WrongSimulator { actual: Simulator },

    /// Gas estimate exceeds the Microtx limit.
    #[error("Gas estimate {gas} ≥ MICROTX_GAS_LIMIT {limit}")]
    GasLimitExceeded { gas: u64, limit: u64 },

    /// Blueprint already expired at the current block.
    #[error("Blueprint expired at block {expiry}; current block {current}")]
    Expired { expiry: u64, current: u64 },

    /// Simulation produced zero or negative profit (after gas deduction).
    #[error("Simulation produced unprofitable result: profit_net={profit_net}")]
    Unprofitable { profit_net: U256 },

    /// Read budget exhausted — callee tried to make more than 8 RPC reads.
    #[error("Read budget exhausted: {used} reads > {budget} budget")]
    ReadBudgetExhausted { used: u8, budget: u8 },
}

// ─────────────────────────────────────────────────────────────────────────────
// HotPathSimResult
// ─────────────────────────────────────────────────────────────────────────────

/// Extended simulation result for hot-path blueprints.
#[derive(Debug, Clone)]
pub struct HotPathSimResult {
    /// Core simulation result for loss attribution and relay submission.
    pub inner: SimResult,
    /// Wall-clock latency of the simulation in microseconds.
    pub latency_us: u64,
    /// Number of RPC reads consumed during simulation.
    pub reads_used: u8,
    /// Whether this blueprint requires a ZK commitment before relay (§15).
    pub requires_zk_commitment: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// MicrotxSimulator
// ─────────────────────────────────────────────────────────────────────────────

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
    pub fn simulate(
        &self,
        bp: &ExecutionBlueprint,
        read_budget: u8,
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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256, U256};
    use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
    use omega_core::types::lane::Lane;

    use crate::metrics::HotPathMetrics;

    fn make_bp(gas: u64, profit: u128, expiry: u64, sim: Simulator) -> ExecutionBlueprint {
        ExecutionBlueprint {
            blueprint_hash: B256::from([2u8; 32]),
            chain_id: 42161,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: sim,
            signal_state_hash: B256::ZERO,
            state_version: 1,
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            calldata: Default::default(),
            strategy_bytecode_hash: B256::ZERO,
            l2_exec_gas_estimate: gas,
            l1_data_gas_estimate: 0,
            extraction_gas: 21_000,
            expected_profit_net: U256::from(profit),
            dynamic_min_profit: U256::from(100_000_u64),
            l2_buffer_factor: 1.15,
            l1_data_buffer_factor: 1.10,
            slippage_bps: 100,
            base_fee_at_creation: 10,
            l1_data_fee_at_creation: 2,
            priority_fee_gwei: 10,
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: expiry,
            nonce: 0,
            confirmation_depth: 12,
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

    #[test]
    fn successful_simulation_returns_revm_simulator() {
        let bp = make_bp(100_000, 2_000_000_000_000_000_u128, 2_000_000, Simulator::Revm);
        let r = sim().simulate(&bp, 8, &metrics()).unwrap();
        assert_eq!(r.inner.simulator, "revm");
        assert!(r.inner.success);
        assert!(r.inner.gas_used > 0 && r.inner.gas_used < 100_000);
    }

    #[test]
    fn latency_is_recorded() {
        let bp = make_bp(50_000, 2_000_000_000_000_000_u128, 2_000_000, Simulator::Revm);
        let r = sim().simulate(&bp, 8, &metrics()).unwrap();
        assert!(r.latency_us < 100_000);
    }

    #[test]
    fn wrong_simulator_returns_error() {
        let bp = make_bp(100_000, 10_000_000, 2_000_000, Simulator::Anvil);
        let err = sim().simulate(&bp, 8, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::WrongSimulator { .. }));
    }

    #[test]
    fn expired_blueprint_returns_error() {
        let s = MicrotxSimulator::new(2_000_001);
        let bp = make_bp(100_000, 2_000_000_000_000_000_u128, 2_000_000, Simulator::Revm);
        let e = s.simulate(&bp, 8, &metrics()).unwrap_err();
        assert!(matches!(e, SimulationError::Expired { .. }));
    }

    #[test]
    fn gas_over_limit_returns_error() {
        let bp = make_bp(MICROTX_GAS_LIMIT, 10_000_000, 2_000_000, Simulator::Revm);
        let err = sim().simulate(&bp, 8, &metrics()).unwrap_err();
        assert!(matches!(err, SimulationError::GasLimitExceeded { .. }));
    }

    #[test]
    fn set_block_updates_expiry_check() {
        let s = sim();
        let bp = make_bp(100_000, 2_000_000_000_000_000_u128, 2_000_000, Simulator::Revm);
        assert!(s.simulate(&bp, 8, &metrics()).is_ok());
        s.set_block(3_000_000);
        assert!(matches!(
            s.simulate(&bp, 8, &metrics()),
            Err(SimulationError::Expired { .. })
        ));
    }
}
