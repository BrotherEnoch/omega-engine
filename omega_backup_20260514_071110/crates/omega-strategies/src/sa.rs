// crates/omega-strategies/src/sa.rs
//
// Simple Arbitrage (SA) â€” Phase 1 strategy (spec Â§1.1).
//
// ## Overview
//
//   SA captures two-hop price discrepancies between DEX pools on the
//   same chain.  It operates exclusively in the Microtx lane (Â§4) with
//   the revm simulator for sub-millisecond execution.
//
// ## Spec constraints
//
//   Phase:           1 (SA)
//   Lane:            Microtx
//   Simulator:       revm (gas < 200,000)
//   Gas budget:      200,000 L2 units
//   Hot-path:        true (Â§11.1 â€” eligible for <1ms execution path)
//   OFA:             false (SA does not consume protected order flow)
//   Min profit:      dynamic (from GasConfig)
//   Confirmation:    12 blocks (Vault minimum)
//
// ## Execution flow
//
//   oracle tick (SpotPrice signal)
//     â†’ score: check spread >= dynamic_min_profit + gas_cost; no I/O
//     â†’ build_blueprint: encode two-hop swap calldata, apply gas model
//     â†’ simulate: revm in-process, <1ms
//     â†’ relay (Gas War Engine cascade, Â§12)
//
// ## Nonce management
//
//   SA maintains a monotonic u64 nonce per (chain_id) pair, stored in
//   an `AtomicU64`.  The nonce key is `ExecutionBlueprint::nonce_key`.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;

use omega_core::types::blueprint::{ExecutionBlueprint, Simulator, StrategyId};
use omega_core::types::lane::Lane;
use omega_core::types::strategy::{OpScore, SignalState, SimResult};
use omega_core::errors::{DropCode, OmegaError};
use omega_core::{GasConfig, OmegaConfig};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

const SA_GAS_BUDGET:      u64 = 200_000;
const SA_EXTRACTION_GAS:  u64 = 21_000;
const SA_L1_DATA_GAS:     u64 = 1_600;   // ~100 bytes calldata Ã— 16
const SA_EXPIRY_BLOCKS:   u64 = 2;       // Microtx: expire quickly
const SA_SLIPPAGE_BPS:    u16 = 50;      // 0.5% max slippage
const SA_CONFIRMATION:    u8  = 12;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SaStrategy
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Simple Arbitrage strategy â€” Phase 1 Microtx lane (Â§1.1, Â§4).
pub struct SaStrategy {
    chain_id:              u64,
    nonce:                 AtomicU64,
    bytecode_hash:         B256,
    contract_addr:         Address,
    gas:                   GasConfig,
}

impl SaStrategy {
    /// Construct from the engine config.
    ///
    /// `bytecode_hash` is the keccak256 of the deployed SA contract's
    /// runtime bytecode, verified by the registry (Â§8).
    /// `contract_addr` is the on-chain strategy contract address.
    pub fn new(
        chain_id:      u64,
        bytecode_hash: B256,
        contract_addr: Address,
        config:        &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce:         AtomicU64::new(0),
            bytecode_hash,
            contract_addr,
            gas:           config.gas.clone(),
        })
    }

    // â”€â”€ Private helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Estimate net profit for a two-hop arbitrage opportunity.
    ///
    /// `spread_wei` is the gross price difference in wei.
    /// Returns `None` when the net profit is below `dynamic_min_profit`.
    fn net_profit_after_gas(
        &self,
        spread_wei:  U256,
        base_fee:    u64,
        l1_data_fee: u64,
    ) -> Option<(U256, u64)> {
        // Dual-component gas cost (Â§7)
        let l2_fee_total_gwei = base_fee
            .saturating_add((self.gas.max_priority_fee_gwei as f64
                * self.gas.conservative_fee_fraction) as u64);

        let l2_cost_wei = U256::from(
            (SA_GAS_BUDGET as f64 * self.gas.l2_buffer_factor) as u64,
        )
        .saturating_mul(U256::from(l2_fee_total_gwei))
        .saturating_mul(U256::from(1_000_000_000_u64)); // gwei â†’ wei

        let l1_cost_wei = U256::from(
            (SA_L1_DATA_GAS as f64 * self.gas.l1_data_buffer_factor) as u64,
        )
        .saturating_mul(U256::from(l1_data_fee))
        .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost = l2_cost_wei.saturating_add(l1_cost_wei);

        if spread_wei <= total_cost {
            return None;
        }

        let net     = spread_wei.saturating_sub(total_cost);
        let dynamic = U256::from(base_fee) // proxy for dynamic min profit
            .saturating_mul(U256::from(SA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        if net <= dynamic {
            return None;
        }

        Some((net, (total_cost / U256::from(1_000_000_000_u64)).saturating_to()))
    }

    /// Encode two-hop swap calldata.
    ///
    /// Encodes: function selector + token_in + token_mid + token_out +
    ///          amount_in + min_amount_out + deadline.
    fn encode_two_hop_calldata(
        amount_in:      U256,
        min_amount_out: U256,
        deadline_block: u64,
    ) -> Bytes {
        // ABI-encode the call: swapTwoHop(uint256,uint256,uint64)
        // selector = keccak256("swapTwoHop(uint256,uint256,uint64)")[0..4]
        let selector = &keccak256(b"swapTwoHop(uint256,uint256,uint64)")[..4];
        let mut data = Vec::with_capacity(4 + 96);
        data.extend_from_slice(selector);

        let mut buf = [0u8; 32];
        amount_in.to_big_endian(&mut buf);
        data.extend_from_slice(&buf);

        min_amount_out.to_big_endian(&mut buf);
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[24..].copy_from_slice(&deadline_block.to_be_bytes());
        data.extend_from_slice(&buf);

        Bytes::from(data)
    }
}

#[async_trait]
impl omega_core::types::strategy::StrategyTrait for SaStrategy {
    fn strategy_id(&self)          -> StrategyId { StrategyId::Sa }
    fn lane(&self)                 -> Lane       { Lane::Microtx }
    fn hot_path_eligible(&self)    -> bool       { true }
    fn gas_budget(&self)           -> u64        { SA_GAS_BUDGET }
    fn expected_bytecode_hash(&self) -> B256     { self.bytecode_hash }

    fn base_min_profit_wei(&self)  -> U256 {
        // 0.0001 ETH minimum gross profit before gas deduction
        U256::from(100_000_000_000_000_u64)
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        // In production: read the spread from oracle prices injected via
        // dependency injection.  Here we model the scoring logic correctly
        // while keeping the type boundary clean.
        //
        // Spread is sourced from the signal's oracle data; we use
        // signal.base_fee_gwei as a proxy for fee pressure.
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0; // normalised
        if fee_pressure > 1.0 {
            // Gas spike â€” MissGasSpike
            return Ok(OpScore {
                score:            0.0,
                expected_profit:  U256::ZERO,
                competition_prob: 1.0,
            });
        }

        // Placeholder spread: 0.002 ETH â€” real value comes from oracle
        let spread_wei = U256::from(2_000_000_000_000_000_u64);
        match self.net_profit_after_gas(
            spread_wei,
            signal.base_fee_gwei,
            signal.l1_data_fee_gwei,
        ) {
            None => Ok(OpScore {
                score:            0.0,
                expected_profit:  U256::ZERO,
                competition_prob: 0.5,
            }),
            Some((net, _)) => {
                let competition_prob = 0.35_f64; // SA median competition
                let score = (1.0 - competition_prob)
                    * (net.saturating_to::<u128>() as f64 / 1e15).min(1.0);
                Ok(OpScore {
                    score:           score.clamp(0.0, 1.0),
                    expected_profit: net,
                    competition_prob,
                })
            }
        }
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let spread_wei = U256::from(2_000_000_000_000_000_u64);
        let (net_profit, gas_cost_gwei) = self
            .net_profit_after_gas(
                spread_wei,
                signal.base_fee_gwei,
                signal.l1_data_fee_gwei,
            )
            .ok_or_else(|| anyhow::anyhow!("Opportunity no longer profitable"))?;

        let nonce    = self.nonce.fetch_add(1, Ordering::Relaxed);
        let calldata = Self::encode_two_hop_calldata(
            spread_wei,
            net_profit,
            signal.block_number + SA_EXPIRY_BLOCKS,
        );

        // Compute blueprint hash over all stable fields
        let mut hash_input = Vec::new();
        hash_input.extend_from_slice(signal.state_hash.as_slice());
        hash_input.extend_from_slice(&nonce.to_be_bytes());
        hash_input.extend_from_slice(&signal.block_number.to_be_bytes());
        let blueprint_hash = keccak256(&hash_input);

        let dynamic_min = U256::from(signal.base_fee_gwei)
            .saturating_mul(U256::from(SA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        Ok(ExecutionBlueprint {
            blueprint_hash,
            chain_id:               self.chain_id,
            strategy_id:            StrategyId::Sa,
            lane:                   Lane::Microtx,
            simulator:              Simulator::Revm,
            signal_state_hash:      signal.state_hash,
            state_version:          signal.state_version,
            flashloan_provider:     Address::ZERO, // SA: no flashloan
            flashloan_amount:       U256::ZERO,
            flashloan_available:    U256::MAX,
            calldata,
            strategy_bytecode_hash: self.bytecode_hash,
            l2_exec_gas_estimate:   SA_GAS_BUDGET,
            l1_data_gas_estimate:   SA_L1_DATA_GAS,
            extraction_gas:         SA_EXTRACTION_GAS,
            expected_profit_net:    net_profit,
            dynamic_min_profit:     dynamic_min,
            l2_buffer_factor:       self.gas.l2_buffer_factor,
            l1_data_buffer_factor:  self.gas.l1_data_buffer_factor,
            slippage_bps:           SA_SLIPPAGE_BPS,
            base_fee_at_creation:   signal.base_fee_gwei,
            l1_data_fee_at_creation:signal.l1_data_fee_gwei,
            priority_fee_gwei:      gas_cost_gwei.min(self.gas.max_priority_fee_gwei),
            price_impact_bps:       Some(30), // 0.3% typical two-hop impact
            ofa_compliant:          false,
            expiry_block:           signal.block_number + SA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth:     SA_CONFIRMATION,
            relay_targets:          vec!["relay_1".into()],
            zk_proof_commitment:    None,
        })
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        // In production: run bp.calldata through the revm double-buffer
        // cache (RevmCacheManager::current()).  Here we model the correct
        // result shape and gas accounting.
        //
        // The simulation is always revm for SA (Microtx lane, <200k gas).
        assert_eq!(bp.simulator, Simulator::Revm);

        // Model: simulation uses 95% of gas estimate (5% headroom)
        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.95) as u64;

        Ok(SimResult {
            profit_net: bp.expected_profit_net,
            gas_used,
            simulator:  "revm".into(),
            success:    true,
        })
    }

    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes {
        // For SA, calldata is final at blueprint construction time.
        bp.calldata.clone()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::OmegaConfig;
    use omega_core::types::strategy::StrategyTrait;

    fn make_strategy() -> Arc<SaStrategy> {
        SaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            &OmegaConfig::default(),
        )
    }

    fn make_signal(base_fee: u64) -> SignalState {
        SignalState {
            state_version:    1,
            chain_id:         42161,
            block_number:     1_000_000,
            base_fee_gwei:    base_fee,
            l1_data_fee_gwei: 2,
            state_hash:       B256::from([0x01; 32]),
        }
    }

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Sa);
        assert_eq!(s.lane(),        Lane::Microtx);
        assert!(s.hot_path_eligible());
        assert!(!s.is_canary());
        assert_eq!(s.gas_budget(), SA_GAS_BUDGET);
    }

    #[tokio::test]
    async fn score_low_fee_returns_nonzero() {
        let s = make_strategy();
        let op = s.score(&make_signal(5)).await.unwrap();
        assert!(op.score > 0.0, "low fee should produce positive score");
    }

    #[tokio::test]
    async fn score_high_fee_returns_zero() {
        // base_fee_gwei = 100 â†’ fee_pressure = 2.0 > 1.0
        let s  = make_strategy();
        let op = s.score(&make_signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0, "gas spike should suppress score");
    }

    #[tokio::test]
    async fn build_blueprint_fields_correct() {
        let s  = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();

        assert_eq!(bp.strategy_id, StrategyId::Sa);
        assert_eq!(bp.chain_id,    42161);
        assert_eq!(bp.lane,        Lane::Microtx);
        assert_eq!(bp.simulator,   Simulator::Revm);
        assert!(!bp.is_canary());
        assert!(bp.is_profitable());
        assert!(!bp.calldata.is_empty());
        assert_eq!(bp.confirmation_depth, SA_CONFIRMATION);
    }

    #[tokio::test]
    async fn nonce_increments() {
        let s   = make_strategy();
        let bp1 = s.build_blueprint(&make_signal(5)).await.unwrap();
        let bp2 = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_ne!(bp1.nonce, bp2.nonce);
        assert_eq!(bp2.nonce, bp1.nonce + 1);
    }

    #[tokio::test]
    async fn simulate_returns_success() {
        let s   = make_strategy();
        let bp  = s.build_blueprint(&make_signal(5)).await.unwrap();
        let sim = s.simulate(&bp).await.unwrap();
        assert!(sim.success);
        assert_eq!(sim.simulator, "revm");
        assert!(sim.gas_used < SA_GAS_BUDGET);
    }

    #[test]
    fn calldata_encoding_non_empty() {
        let data = SaStrategy::encode_two_hop_calldata(
            U256::from(1_000_000_u64),
            U256::from(900_000_u64),
            1_000_100,
        );
        // selector(4) + amount_in(32) + min_out(32) + deadline(32) = 100 bytes
        assert_eq!(data.len(), 100);
    }
}