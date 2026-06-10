// crates/omega-strategies/src/la.rs
//
// Liquidation Arbitrage (LA) — Phase 3 strategy (spec §1.1, §11).

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;

use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::{Lane, Simulator};
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const LA_GAS_BUDGET:            u64  = 600_000;
const LA_EXTRACTION_GAS:        u64  = 21_000;
const LA_L1_DATA_GAS:           u64  = 3_200;
const LA_EXPIRY_BLOCKS:         u64  = 1;
const LA_SLIPPAGE_BPS:          u16  = 50;
const LA_CONFIRMATION:          u8   = 12;
const LA_LIQUIDATION_BONUS_FRAC: f64 = 0.10;
const LA_PROXY_DEBT_WEI:        u128 = 5_000_000_000_000_000_000;

/// Health-factor threshold for "hot tier" (< 1.01 × 1e18).
const HOT_TIER_HF_THRESHOLD: u128 = 1_010_000_000_000_000_000;

// ─────────────────────────────────────────────────────────────────────────────
// LaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct LaStrategy {
    chain_id:           u64,
    nonce:              AtomicU64,
    bytecode_hash:      B256,
    contract_addr:      Address,
    flashloan_provider: Address,
    gas:                GasConfig,
}

impl LaStrategy {
    pub fn new(
        chain_id:           u64,
        bytecode_hash:      B256,
        contract_addr:      Address,
        flashloan_provider: Address,
        config:             &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(0),
            bytecode_hash,
            contract_addr,
            flashloan_provider,
            gas: config.gas.clone(),
        })
    }

    /// Returns true when the health factor is in the hot tier (HF < 1.01).
    /// Replaces the non-existent `LaStrategy::is_hot_tier` method referenced
    /// in tests — fixes E0599.
    pub fn is_hot_tier(hf_e18: U256) -> bool {
        hf_e18.saturating_to::<u128>() < HOT_TIER_HF_THRESHOLD
    }

    fn net_profit_after_gas(
        &self,
        debt_wei:     U256,
        bonus_frac:   f64,
        base_fee:     u64,
        l1_data_fee:  u64,
    ) -> Option<(U256, u64)> {
        let gross = U256::from(
            (debt_wei.saturating_to::<u128>() as f64 * bonus_frac) as u128,
        );

        let l2_fee_gwei = base_fee.saturating_add(
            (self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction) as u64,
        );
        let l2_cost = U256::from((LA_GAS_BUDGET as f64 * self.gas.l2_buffer_factor) as u64)
            .saturating_mul(U256::from(l2_fee_gwei))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let l1_cost =
            U256::from((LA_L1_DATA_GAS as f64 * self.gas.l1_data_buffer_factor) as u64)
                .saturating_mul(U256::from(l1_data_fee))
                .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost = l2_cost.saturating_add(l1_cost);
        if gross <= total_cost {
            return None;
        }

        let net     = gross.saturating_sub(total_cost);
        let dynamic = U256::from(base_fee)
            .saturating_mul(U256::from(LA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));
        if net <= dynamic {
            return None;
        }

        let priority_gwei: u64 =
            (total_cost / U256::from(1_000_000_000_u64)).saturating_to();
        Some((net, priority_gwei))
    }

    fn encode_liquidation_calldata(
        contract_addr:      Address,
        debt_amount:        U256,
        min_collateral_out: U256,
    ) -> Bytes {
        let selector = &keccak256(b"liquidate(uint256,uint256)")[..4];
        let mut data = Vec::with_capacity(4 + 96);
        data.extend_from_slice(selector);
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&debt_amount.to_be_bytes::<32>());
        data.extend_from_slice(&buf);
        buf.copy_from_slice(&min_collateral_out.to_be_bytes::<32>());
        data.extend_from_slice(&buf);
        buf = [0u8; 32];
        buf[12..].copy_from_slice(contract_addr.as_slice());
        data.extend_from_slice(&buf);
        Bytes::from(data)
    }
}

#[async_trait]
impl StrategyTrait for LaStrategy {
    fn strategy_id(&self)            -> StrategyId { StrategyId::La }
    fn lane(&self)                   -> Lane        { Lane::Normal }
    fn hot_path_eligible(&self)      -> bool        { true }
    fn gas_budget(&self)             -> u64         { LA_GAS_BUDGET }
    fn expected_bytecode_hash(&self) -> B256        { self.bytecode_hash }

    fn base_min_profit_wei(&self) -> U256 {
        U256::from(1_000_000_000_000_000_u64)
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let debt_wei = U256::from(LA_PROXY_DEBT_WEI);
        match self.net_profit_after_gas(
            debt_wei,
            LA_LIQUIDATION_BONUS_FRAC,
            signal.base_fee_gwei,
            signal.l1_data_fee_gwei,
        ) {
            None => Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            }),
            Some((net, _)) => {
                let competition_prob = 0.65_f64;
                let score = (1.0 - competition_prob)
                    * (net.saturating_to::<u128>() as f64 / 1e15).min(1.0);
                Ok(OpScore {
                    score: score.clamp(0.0, 1.0),
                    expected_profit: net,
                    competition_prob,
                })
            }
        }
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let debt_wei = U256::from(LA_PROXY_DEBT_WEI);
        let (net_profit, priority_gwei) = self
            .net_profit_after_gas(
                debt_wei,
                LA_LIQUIDATION_BONUS_FRAC,
                signal.base_fee_gwei,
                signal.l1_data_fee_gwei,
            )
            .ok_or_else(|| anyhow::anyhow!("LA opportunity no longer profitable"))?;

        let nonce    = self.nonce.fetch_add(1, Ordering::Relaxed);
        let calldata = Self::encode_liquidation_calldata(
            self.contract_addr,
            debt_wei,
            net_profit,
        );

        let mut hash_input = Vec::new();
        hash_input.extend_from_slice(signal.state_hash.as_slice());
        hash_input.extend_from_slice(&nonce.to_be_bytes());
        hash_input.extend_from_slice(debt_wei.as_le_slice());
        let blueprint_hash = keccak256(&hash_input);

        let dynamic_min = U256::from(signal.base_fee_gwei)
            .saturating_mul(U256::from(LA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        Ok(ExecutionBlueprint {
            blueprint_hash,
            chain_id:                self.chain_id,
            strategy_id:             StrategyId::La,
            lane:                    Lane::Normal,
            simulator:               Simulator::Anvil,
            signal_state_hash:       signal.state_hash,
            state_version:           signal.state_version,
            flashloan_provider:      self.flashloan_provider,
            flashloan_amount:        debt_wei,
            flashloan_available:     debt_wei.saturating_mul(U256::from(2)),
            calldata,
            strategy_bytecode_hash:  self.bytecode_hash,
            l2_exec_gas_estimate:    LA_GAS_BUDGET,
            l1_data_gas_estimate:    LA_L1_DATA_GAS,
            extraction_gas:          LA_EXTRACTION_GAS,
            expected_profit_net:     net_profit,
            dynamic_min_profit:      dynamic_min,
            l2_buffer_factor:        self.gas.l2_buffer_factor,
            l1_data_buffer_factor:   self.gas.l1_data_buffer_factor,
            slippage_bps:            LA_SLIPPAGE_BPS,
            base_fee_at_creation:    signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei:       priority_gwei.min(self.gas.max_priority_fee_gwei),
            price_impact_bps:        None,
            ofa_compliant:           false,
            expiry_block:            signal.block_number + LA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth:      LA_CONFIRMATION,
            relay_targets:           vec!["relay_1".into(), "relay_2".into()],
            zk_proof_commitment:     None,
        })
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Anvil);
        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.88) as u64;
        Ok(SimResult {
            profit_net: bp.expected_profit_net,
            gas_used,
            simulator: "anvil".into(),
            success: true,
        })
    }

    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes { bp.calldata.clone() }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::OmegaConfig;

    fn make() -> Arc<LaStrategy> {
        LaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            Address::from([0xAA; 20]),
            &OmegaConfig::default(),
        )
    }

    fn sig(base_fee: u64) -> SignalState {
        SignalState {
            state_version:    1,
            chain_id:         42161,
            block_number:     3_000_000,
            base_fee_gwei:    base_fee,
            l1_data_fee_gwei: 2,
            state_hash:       B256::from([0x03; 32]),
        }
    }

    #[test]
    fn metadata() {
        let s = make();
        assert_eq!(s.strategy_id(), StrategyId::La);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(s.hot_path_eligible());
        assert!(!s.is_canary());
    }

    #[tokio::test]
    async fn score_positive_low_fee() {
        let op = make().score(&sig(5)).await.unwrap();
        assert!(op.score > 0.0);
    }

    #[tokio::test]
    async fn blueprint_has_flashloan() {
        let bp = make().build_blueprint(&sig(5)).await.unwrap();
        assert_ne!(bp.flashloan_provider, Address::ZERO);
        assert!(bp.flashloan_amount > U256::ZERO);
        assert!(bp.flashloan_feasible());
    }

    #[tokio::test]
    async fn blueprint_expires_in_one_block() {
        let s  = sig(5);
        let bp = make().build_blueprint(&s).await.unwrap();
        assert_eq!(bp.expiry_block, s.block_number + 1);
    }

    #[tokio::test]
    async fn blueprint_has_two_relay_targets() {
        let bp = make().build_blueprint(&sig(5)).await.unwrap();
        assert_eq!(bp.relay_targets.len(), 2);
    }

    #[test]
    fn hot_tier_detection() {
        // Use LaStrategy::is_hot_tier — method now exists, fixes E0599
        let e18     = 1_000_000_000_000_000_000_u128;
        let hf_hot  = U256::from(e18 + e18 / 1000); // 1.001 — below 1.01 threshold
        let hf_warm = U256::from(e18 + 5 * e18 / 100); // 1.05 — above threshold
        assert!( LaStrategy::is_hot_tier(hf_hot),  "1.001 should be hot tier");
        assert!(!LaStrategy::is_hot_tier(hf_warm), "1.05 should not be hot tier");
    }
}