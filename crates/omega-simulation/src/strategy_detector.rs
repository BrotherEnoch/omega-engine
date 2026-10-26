// crates/omega-simulation/src/strategy_detector.rs
//! Real strategy-driven detector for Phase 0.5 simulation.
//! CNRY: score-only (never builds blueprints — by design).
//! SA:   score → build_blueprint → map to simulation Opportunity.

use std::sync::Arc;

use alloy_primitives::{Address as AlloyAddress, B256, U256 as AlloyU256};
use async_trait::async_trait;
use ethers::types::{Address, Bytes, U256};
use omega_core::types::strategy::{SignalState, StrategyTrait};
use omega_core::OmegaConfig;
use omega_flashloan::{FlashloanProvider, LiquidityRegistry};
use omega_strategies::{CnryStrategy, SaStrategy};

use crate::error::Result;
use crate::traits::{Opportunity, OpportunityDetector, OpportunityKind};

const ARBITRUM_CHAIN_ID: u64 = 42161;

/// Arbitrum canonical WETH (matches SA).
fn arbitrum_weth() -> AlloyAddress {
    // 0x82aF49447D8a07e3bd95BD0d56f35241523fBab1
    AlloyAddress::from([
        0x82, 0xAF, 0x49, 0x44, 0x7D, 0x8A, 0x07, 0xE3, 0xBD, 0x95, 0xBD, 0x0D, 0x56, 0xF3, 0x52,
        0x41, 0x52, 0x3F, 0xBA, 0xB1,
    ])
}

pub struct StrategyDetector {
    cnry: Arc<CnryStrategy>,
    sa: Arc<SaStrategy>,
    /// Incremented each cycle so state_version is monotonic.
    state_version: u64,
}

impl StrategyDetector {
    pub fn new(config: &OmegaConfig) -> Self {
        let liquidity_registry = LiquidityRegistry::new();
        // Seed Balancer WETH liquidity so SA can select a provider
        // (same pattern as omega-strategies SA unit tests).
        liquidity_registry.update(
            ARBITRUM_CHAIN_ID,
            FlashloanProvider::Balancer,
            arbitrum_weth(),
            AlloyAddress::from([0xBA; 20]), // placeholder Balancer vault on fork
            AlloyU256::from(10_000_000_000_000_000_000u128), // 10 ETH
            1,
        );

        let cnry = CnryStrategy::new(ARBITRUM_CHAIN_ID, config);
        let sa = SaStrategy::new(
            ARBITRUM_CHAIN_ID,
            B256::from([0xAB; 32]), // placeholder bytecode hash until real deploy
            AlloyAddress::ZERO,     // placeholder; replace after deploy on fork
            liquidity_registry,
            config,
        );

        Self {
            cnry,
            sa,
            state_version: 0,
        }
    }

    fn make_signal(&mut self, block_number: u64, base_fee_gwei: u64) -> SignalState {
        self.state_version = self.state_version.saturating_add(1);
        SignalState {
            state_version: self.state_version,
            chain_id: ARBITRUM_CHAIN_ID,
            block_number,
            base_fee_gwei,
            l1_data_fee_gwei: 2, // conservative default; improve later from oracle
            state_hash: B256::from([0x01; 32]),
            mev_share_events_in_window: 0,
        }
    }
}

#[async_trait]
impl OpportunityDetector for StrategyDetector {
    async fn next_opportunities(&mut self, block_number: u64) -> Result<Vec<Opportunity>> {
        // Arbitrum base fee is often sub-gwei; strategies treat whole gwei.
        // Use a low value so SA/CNRY can produce non-zero scores in sim.
        let base_fee_gwei: u64 = 1;
        let signal = self.make_signal(block_number, base_fee_gwei);

        // CNRY: score only (never builds a blueprint)
        match self.cnry.score(&signal).await {
            Ok(op) => {
                tracing::info!(
                    strategy = "CNRY",
                    block = block_number,
                    score = op.score,
                    expected_profit = %op.expected_profit,
                    "canary scored"
                );
            }
            Err(e) => tracing::warn!(strategy = "CNRY", error = %e, "canary score failed"),
        }

        let mut out = Vec::new();

        // SA: score → if proceeds, build blueprint → map to Opportunity
        match self.sa.score(&signal).await {
            Ok(op) if op.should_proceed() => match self.sa.build_blueprint(&signal).await {
                Ok(bp) => {
                    tracing::info!(
                        strategy = "SA",
                        block = block_number,
                        expected_profit_net = %bp.expected_profit_net,
                        "SA blueprint built"
                    );

                    // Map alloy types → ethers types used by the simulation crate
                    let target = Address::from_slice(bp.provider_contract.as_slice());
                    let asset = Address::from_slice(bp.flashloan_token.as_slice());
                    let amount = U256::from_dec_str(&bp.flashloan_amount.to_string())
                        .unwrap_or(U256::zero());
                    let profit = U256::from_dec_str(&bp.expected_profit_net.to_string())
                        .unwrap_or(U256::zero());
                    let gas = U256::from(bp.l2_exec_gas_estimate);
                    let calldata = Bytes::from(bp.calldata.to_vec());

                    out.push(Opportunity {
                        id: format!("sa-{}-{}", block_number, bp.nonce),
                        kind: OpportunityKind::Arbitrage,
                        target_pool: target,
                        flash_loan_asset: asset,
                        flash_loan_amount: amount,
                        calldata,
                        expected_profit_wei: profit,
                        gas_estimate: gas,
                    });
                }
                Err(e) => {
                    tracing::warn!(strategy = "SA", error = %e, "build_blueprint failed");
                }
            },
            Ok(op) => {
                tracing::debug!(
                    strategy = "SA",
                    score = op.score,
                    "SA below threshold — no blueprint"
                );
            }
            Err(e) => tracing::warn!(strategy = "SA", error = %e, "score failed"),
        }

        Ok(out)
    }
}
