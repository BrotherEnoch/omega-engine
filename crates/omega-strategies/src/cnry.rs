// crates/omega-strategies/src/cnry.rs
//
// Canary (CNRY) — Phase 0 signal validator (spec §1.1).
//
// ## Audit note (this revision)
//
// `build_blueprint` here never actually constructs an `ExecutionBlueprint`
// — it always returns `Err(MissWhitelist)` before reaching that point, by
// design (CNRY must never produce a real blueprint). So none of the
// `blueprint_hash` inconsistency issues fixed in sa.rs/la.rs/msa.rs/mev.rs
// apply to this file's production code. The only change needed here is
// adding the three new `ExecutionBlueprint` fields (`signal_id`,
// `client_order_id`, `idempotency_key`) to the two test-only blueprint
// literals below, since `ExecutionBlueprint` now requires them at every
// construction site.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use alloy_primitives::{B256, Bytes, U256};
use anyhow::Result;
use async_trait::async_trait;

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::Lane;
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const CNRY_GAS_BUDGET:    u64  = 0;
const CNRY_BYTECODE_HASH: B256 = B256::ZERO;
const CNRY_SPREAD_WEI:    u128 = 200_000_000_000_000_000; // 0.2 ETH

// ─────────────────────────────────────────────────────────────────────────────
// CnryStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct CnryStrategy {
    chain_id:     u64,
    scored_count: AtomicU64,
    gas:          GasConfig,
}

impl CnryStrategy {
    pub fn new(chain_id: u64, config: &OmegaConfig) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            scored_count: AtomicU64::new(0),
            gas: config.gas.clone(),
        })
    }

    pub fn scored_count(&self) -> u64 {
        self.scored_count.load(Ordering::Relaxed)
    }

    fn compute_score(&self, signal: &SignalState) -> OpScore {
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0;
        if fee_pressure > 1.0 {
            return OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            };
        }

        let spread_wei  = U256::from(CNRY_SPREAD_WEI);
        let l2_cost_gwei = (200_000_f64
            * self.gas.l2_buffer_factor
            * (signal.base_fee_gwei as f64
                + self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction))
            as u64;
        let cost_wei = U256::from(l2_cost_gwei).saturating_mul(U256::from(1_000_000_000_u64));

        if spread_wei <= cost_wei {
            return OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.5,
            };
        }

        let net              = spread_wei.saturating_sub(cost_wei);
        let competition_prob = 0.35_f64;
        let score = (1.0 - competition_prob)
            * (net.saturating_to::<u128>() as f64 / 1e15).min(1.0);

        OpScore {
            score: score.clamp(0.0, 1.0),
            expected_profit: net,
            competition_prob,
        }
    }
}

#[async_trait]
impl StrategyTrait for CnryStrategy {
    fn strategy_id(&self)           -> StrategyId { StrategyId::Cnry }
    fn lane(&self)                  -> Lane        { Lane::Microtx }
    fn hot_path_eligible(&self)     -> bool        { false }
    fn gas_budget(&self)            -> u64         { CNRY_GAS_BUDGET }
    fn expected_bytecode_hash(&self)-> B256        { CNRY_BYTECODE_HASH }
    fn is_canary(&self)             -> bool        { true }

    fn base_min_profit_wei(&self) -> U256 { U256::ZERO }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let op = self.compute_score(signal);
        self.scored_count.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            chain_id     = self.chain_id,
            block_number = signal.block_number,
            score        = op.score,
            "CNRY scored opportunity",
        );
        Ok(op)
    }

    async fn build_blueprint(&self, _signal: &SignalState) -> Result<ExecutionBlueprint> {
        Err(anyhow::anyhow!(OmegaError::dropped(DropCode::MissWhitelist)))
    }

    async fn simulate(&self, _bp: &ExecutionBlueprint) -> Result<SimResult> {
        Err(anyhow::anyhow!(OmegaError::dropped(DropCode::MissWhitelist)))
    }

    fn encode_calldata(&self, _bp: &ExecutionBlueprint) -> Bytes { Bytes::new() }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    // Address was missing — fixes E0433 at lines 261 and 299
    use alloy_primitives::Address;
    use omega_core::OmegaConfig;
    use uuid::Uuid;

    fn make_strategy() -> Arc<CnryStrategy> {
        CnryStrategy::new(42161, &OmegaConfig::default())
    }

    fn signal(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 1,
            chain_id:      42161,
            block_number:  1_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash:    B256::from([0x01; 32]),
        }
    }

    /// Test-only blueprint literal. CNRY's real `build_blueprint` never
    /// constructs one of these (see module doc comment), so signal_id/
    /// client_order_id/idempotency_key here are just placeholders to
    /// satisfy the struct's field list — not integrity-checked values.
    /// Do not copy this pattern into code that DOES rely on
    /// verify_hash()/verify_idempotency_key().
    fn test_blueprint() -> ExecutionBlueprint {
        let signal_id = Uuid::from_bytes([0x00u8; 16]);
        let client_order_id =
            ExecutionBlueprint::derive_client_order_id(StrategyId::Cnry, 42161, 0, signal_id);
        ExecutionBlueprint {
            blueprint_hash:          B256::ZERO,
            chain_id:                42161,
            strategy_id:             StrategyId::Cnry,
            lane:                    Lane::Microtx,
            simulator:               omega_core::types::lane::Simulator::Revm,
            signal_state_hash:       B256::ZERO,
            state_version:           0,
            signal_id,
            flashloan_provider:      Address::ZERO,
            flashloan_amount:        U256::ZERO,
            flashloan_available:     U256::ZERO,
            calldata:                Bytes::new(),
            strategy_bytecode_hash:  B256::ZERO,
            l2_exec_gas_estimate:    0,
            l1_data_gas_estimate:    0,
            extraction_gas:          0,
            expected_profit_net:     U256::ZERO,
            dynamic_min_profit:      U256::ZERO,
            l2_buffer_factor:        1.0,
            l1_data_buffer_factor:   1.0,
            slippage_bps:            0,
            base_fee_at_creation:    0,
            l1_data_fee_at_creation: 0,
            priority_fee_gwei:       0,
            price_impact_bps:        None,
            ofa_compliant:           false,
            expiry_block:            0,
            nonce:                   0,
            confirmation_depth:      12,
            client_order_id,
            idempotency_key:         B256::ZERO,
            relay_targets:           vec![],
            zk_proof_commitment:     None,
        }
    }

    #[test]
    fn is_canary() {
        let s = make_strategy();
        assert!(s.is_canary());
        assert_eq!(s.strategy_id(), StrategyId::Cnry);
        assert!(!s.hot_path_eligible());
        assert_eq!(s.gas_budget(), 0);
    }

    #[tokio::test]
    async fn score_low_fee_positive() {
        let s  = make_strategy();
        let op = s.score(&signal(5)).await.unwrap();
        assert!(op.score > 0.0);
        assert_eq!(s.scored_count(), 1);
    }

    #[tokio::test]
    async fn score_high_fee_zero() {
        let s  = make_strategy();
        let op = s.score(&signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn build_blueprint_blocked() {
        let s   = make_strategy();
        let err = s.build_blueprint(&signal(5)).await;
        assert!(err.is_err(), "CNRY must never build blueprints");
    }

    #[tokio::test]
    async fn simulate_blocked() {
        let s  = make_strategy();
        let bp = test_blueprint();
        assert!(s.simulate(&bp).await.is_err(), "CNRY must never simulate");
    }

    #[test]
    fn encode_calldata_empty() {
        let s  = make_strategy();
        let bp = test_blueprint();
        assert!(s.encode_calldata(&bp).is_empty());
    }
}