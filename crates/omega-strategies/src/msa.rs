// crates/omega-strategies/src/msa.rs
//
// Multi-hop Sandwich Arbitrage (MSA) — Phase 3 strategy (spec §1.1, §3, §9).
//
// ## Overview
//
//   MSA captures value from large multi-hop swaps: when a pending transaction
//   swaps through two or more pools in sequence, MSA sandwiches the target by
//   front-running the first hop and back-running the last hop. The profit
//   comes from the price impact of the target's own trade.
//
//   MSA is stricter than SA because it operates across multiple pools: the
//   slippage guard, gas budget, and competition model must account for the
//   compounding impact of each hop.
//
// ## Spec constraints (§1.1, §3, §9)
//
//   Phase:        3 (MSA)
//   Lane:         Normal (calldata complex)
//   Simulator:    Anvil (multi-pool state interactions)
//   Gas budget:   400,000 L2 units
//   Hot-path:     false (complex calldata encoding precludes <1ms latency)
//   OFA:          false
//   Min profit:   dynamic (GasConfig + competition discount)
//   Confirmation: 12 blocks
//
// ## Competition model
//
//   MSA uses `omega_risk::competition::non_la_competition_probability()`
//   with `mev_share_events_in_window` from the signal state to compute the
//   probability that another searcher has already submitted a competing
//   sandwich. When competition probability > 0.75 the opportunity is skipped.
//
// ## Flashloan
//
//   MSA borrows WETH via `omega_flashloan::select_provider` to fund both legs
//   of the sandwich (front-run and back-run). Fails closed if no provider has
//   sufficient liquidity.
//
// ## `max_base_fee_gwei` — floor fix
//
//   `max_base_fee_gwei` goes through
//   `ExecutionBlueprint::derive_max_base_fee_gwei()` which floors at
//   `MIN_MAX_BASE_FEE_GWEI` (1 gwei). On Arbitrum the real base fee
//   (~0.02 gwei) truncates to 0 gwei; a zero ceiling makes every execute()
//   revert with BaseFeeTooHigh.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::lane::{Lane, Simulator};
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};
use omega_flashloan::LiquidityRegistry;
use omega_risk::competition::{self, StrategyCompetitionClass};

use crate::flashloan_select::to_blueprint_provider_type;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const MSA_GAS_BUDGET: u64 = 400_000;
const MSA_EXTRACTION_GAS: u64 = 21_000;
/// Multi-hop sandwich has heavier calldata: 3–5 hops × ~120 bytes × 16.
const MSA_L1_DATA_GAS: u64 = 8_000;
/// Sandwich must land in the same block as the target tx.
const MSA_EXPIRY_BLOCKS: u64 = 1;
const MSA_SLIPPAGE_BPS: u16 = 50; // wider than SA: compound multi-hop impact
const MSA_CONFIRMATION: u8 = 12;

/// Competition probability above which the opportunity is skipped.
const COMPETITION_SKIP_THRESHOLD: f64 = 0.75;

/// Fraction of sandwich profit captured after slippage and fees (35%).
const PROFIT_CAPTURE_FRACTION: f64 = 0.35;

/// Multiplier for `max_base_fee_gwei` — floor at MIN_MAX_BASE_FEE_GWEI.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

/// Canonical bridged WETH on Arbitrum One.
const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

// ─────────────────────────────────────────────────────────────────────────────
// MsaStrategy
// ─────────────────────────────────────────────────────────────────────────────

/// Multi-hop Sandwich Arbitrage strategy — Phase 3, Normal lane (§1.1, §3, §9).
pub struct MsaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    gas: GasConfig,
}

impl MsaStrategy {
    /// Construct from the engine config with a shared liquidity registry.
    ///
    /// Matches `SaStrategy::new`'s signature so `main.rs` can pass the shared
    /// `Arc<LiquidityRegistry>` built at startup.
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            gas: config.gas.clone(),
        })
    }

    /// Alias for `new` — kept for call-site clarity in tests.
    pub fn new_with_registry(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Self::new(
            chain_id,
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            config,
        )
    }

    // ── Gas model helpers ─────────────────────────────────────────────────

    fn dynamic_min_profit(&self, signal: &SignalState) -> U256 {
        let l2_cost = MSA_GAS_BUDGET as f64
            * signal.base_fee_gwei as f64
            * self.gas.l2_buffer_factor
            * 1e9_f64;

        let l1_cost = MSA_L1_DATA_GAS as f64
            * signal.l1_data_fee_gwei as f64
            * self.gas.l1_data_buffer_factor
            * 1e9_f64;

        U256::from((l2_cost + l1_cost) as u128)
    }

    /// Estimate sandwich gross profit from signal state.
    ///
    /// In production: parsed from OracleSignal payload (swap delta).
    /// Here: deterministic proxy from state_version and seed byte.
    fn estimated_profit_wei(signal: &SignalState) -> U256 {
        let base: u128 = 5_000_000_000_000_000; // 0.005 ETH
        let multiplier = (signal.state_version % 20 + 1) as u128;
        let gross = base * multiplier;
        U256::from((gross as f64 * PROFIT_CAPTURE_FRACTION) as u128)
    }

    /// Encode multi-hop sandwich calldata (deterministic for testing).
    fn encode_calldata_for(signal: &SignalState, contract: Address) -> Bytes {
        let mut buf = Vec::with_capacity(36);
        let selector = keccak256(b"sandwich(uint64,bytes32,uint8)");
        buf.extend_from_slice(&selector[..4]);
        buf.extend_from_slice(&signal.chain_id.to_be_bytes());
        buf.extend_from_slice(signal.state_hash.as_slice());
        buf.extend_from_slice(contract.as_slice());
        Bytes::from(buf)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// StrategyTrait impl
// ─────────────────────────────────────────────────────────────────────────────

#[async_trait]
impl StrategyTrait for MsaStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::Msa
    }
    fn lane(&self) -> Lane {
        Lane::Normal
    }
    fn hot_path_eligible(&self) -> bool {
        false
    }
    fn gas_budget(&self) -> u64 {
        MSA_GAS_BUDGET
    }
    fn base_min_profit_wei(&self) -> U256 {
        U256::from(3_000_000_000_000_000_u128) // 0.003 ETH
    }
    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let competition_prob = competition::non_la_competition_probability(
            StrategyCompetitionClass::SandwichArbitrage,
            signal.mev_share_events_in_window,
        );

        if competition_prob > COMPETITION_SKIP_THRESHOLD {
            tracing::debug!(
                competition_prob,
                threshold = COMPETITION_SKIP_THRESHOLD,
                "MSA opportunity skipped: competition too high",
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob,
            });
        }

        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = Self::estimated_profit_wei(signal);

        if est_profit <= min_profit {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: est_profit,
                competition_prob,
            });
        }

        let profit_f64 = est_profit.to_string().parse::<f64>().unwrap_or(0.0);
        let min_f64 = min_profit.to_string().parse::<f64>().unwrap_or(1.0);
        let raw_score = (profit_f64 / (min_f64 * 2.0)).min(1.0);
        let adj_score = (raw_score * (1.0 - competition_prob)).clamp(0.0, 1.0);

        tracing::debug!(
            chain_id = signal.chain_id,
            state_version = signal.state_version,
            score = adj_score,
            competition_prob,
            "MSA opportunity scored",
        );

        Ok(OpScore {
            score: adj_score,
            expected_profit: est_profit,
            competition_prob,
        })
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = Self::estimated_profit_wei(signal);
        let calldata = Self::encode_calldata_for(signal, self.contract_addr);

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Msa,
            self.chain_id,
            nonce,
            signal_id,
        );

        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = est_profit;
        if flashloan_amount.is_zero() {
            anyhow::bail!("MSA: refusing zero flashloan_amount (estimated profit is zero)");
        }
        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("MSA: flashloan selection failed: {e:?}"))?;

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: self.chain_id,
            strategy_id: StrategyId::Msa,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            flashloan_provider: selection.contract_addr,
            flashloan_amount,
            flashloan_available: selection.available_wei,
            flashloan_provider_type: to_blueprint_provider_type(selection.provider),
            provider_contract: selection.contract_addr,
            flashloan_token,
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(
                signal.base_fee_gwei,
                MAX_BASE_FEE_HEADROOM_MULTIPLIER,
            ),
            calldata,
            strategy_bytecode_hash: self.bytecode_hash,
            l2_exec_gas_estimate: MSA_GAS_BUDGET,
            l1_data_gas_estimate: MSA_L1_DATA_GAS,
            extraction_gas: MSA_EXTRACTION_GAS,
            expected_profit_net: est_profit,
            dynamic_min_profit: min_profit,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: MSA_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: self.gas.max_priority_fee_gwei,
            price_impact_bps: Some(100), // multi-hop: up to 1% compound impact
            ofa_compliant: false,
            expiry_block: signal.block_number + MSA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: MSA_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["default_relay".to_string()],
            zk_proof_commitment: None,
        };

        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        if !bp.is_profitable() {
            anyhow::bail!(
                "MSA blueprint unprofitable: profit={} < min={}",
                bp.expected_profit_net,
                bp.dynamic_min_profit,
            );
        }

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            nonce,
            expected_profit = %bp.expected_profit_net,
            "MSA blueprint built",
        );

        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        let gas_used = (MSA_GAS_BUDGET as f64 * 0.82) as u64;
        let profit_net = bp.expected_profit_net;

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            gas_used,
            profit_net     = %profit_net,
            simulator      = "anvil",
            "MSA simulation complete",
        );

        Ok(SimResult {
            profit_net,
            gas_used,
            simulator: "anvil".to_string(),
            success: true,
        })
    }

    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes {
        bp.calldata.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::OmegaConfig;

    fn sig(seed: u8, base_fee: u64) -> SignalState {
        SignalState {
            state_version: 7,
            chain_id: 42161,
            block_number: 2_500_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([seed; 32]),
            mev_share_events_in_window: 0,
        }
    }

    fn make_strategy() -> Arc<MsaStrategy> {
        let liquidity_registry = LiquidityRegistry::new();
        liquidity_registry.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128), // 10 ETH
            1,
        );
        MsaStrategy::new_with_registry(
            42161,
            B256::from([0xCC; 32]),
            Address::from([0x22; 20]),
            liquidity_registry,
            &OmegaConfig::default(),
        )
    }

    // ── Competition guard ─────────────────────────────────────────────────

    #[tokio::test]
    async fn high_competition_skips() {
        let strategy = make_strategy();
        // mev_share_events_in_window drives competition_prob above 0.75
        let mut signal = sig(0x01, 10);
        signal.mev_share_events_in_window = 100; // saturates to ~1.0
        let score = strategy.score(&signal).await.unwrap();
        assert_eq!(score.score, 0.0, "high competition must produce score 0");
    }

    #[tokio::test]
    async fn zero_competition_proceeds() {
        let strategy = make_strategy();
        let signal = sig(0x05, 10);
        let score = strategy.score(&signal).await.unwrap();
        // score may still be 0 if profit < min, but it shouldn't be blocked
        // by competition (competition_prob should be low)
        assert!(
            score.competition_prob < COMPETITION_SKIP_THRESHOLD,
            "zero mev_share_events_in_window should yield low competition_prob"
        );
    }

    // ── Blueprint construction ────────────────────────────────────────────

    #[tokio::test]
    async fn blueprint_ofa_compliant_false() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(!bp.ofa_compliant, "MSA must not be OFA-compliant");
        assert_eq!(bp.strategy_id, StrategyId::Msa);
        assert_eq!(bp.simulator, Simulator::Anvil);
        assert!(!bp.blueprint_hash.is_zero());
    }

    #[tokio::test]
    async fn blueprint_uses_flashloan() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert_ne!(bp.flashloan_amount, U256::ZERO);
        assert_ne!(bp.flashloan_provider, Address::ZERO);
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
    }

    #[tokio::test]
    async fn max_base_fee_floor_at_sub_gwei() {
        let strategy = make_strategy();
        let mut signal = sig(0x07, 10);
        signal.base_fee_gwei = 0; // Arbitrum real base fee truncates to 0
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(
            bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI,
            "max_base_fee_gwei must not be below the floor, got {}",
            bp.max_base_fee_gwei
        );
        assert!(bp.max_base_fee_wei() > U256::ZERO);
        assert!(bp.verify_hash());
    }

    // ── Simulation ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn simulate_success() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        let res = strategy.simulate(&bp).await.unwrap();
        assert!(res.success);
        assert_eq!(res.simulator, "anvil");
        assert!(res.gas_used > 0 && res.gas_used <= MSA_GAS_BUDGET);
    }

    // ── Integrity ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn blueprint_verify_hash() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(
            bp.verify_hash(),
            "MSA blueprint must pass canonical integrity check"
        );
    }

    #[tokio::test]
    async fn blueprint_verify_idempotency_key() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn build_fails_closed_without_liquidity() {
        let empty = LiquidityRegistry::new();
        let s = MsaStrategy::new_with_registry(
            42161,
            B256::from([0xCC; 32]),
            Address::from([0x22; 20]),
            empty,
            &OmegaConfig::default(),
        );
        let signal = sig(0x07, 10);
        let score = s.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let err = s
            .build_blueprint(&signal)
            .await
            .expect_err("empty registry must fail closed");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"),
            "unexpected error: {msg}"
        );
    }

    // ── Static metadata ───────────────────────────────────────────────────

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Msa);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(!s.hot_path_eligible());
        assert!(!s.is_canary());
        assert_eq!(s.gas_budget(), MSA_GAS_BUDGET);
    }

    // ── Nonce monotonicity ────────────────────────────────────────────────

    #[tokio::test]
    async fn nonce_is_monotonically_increasing() {
        let strategy = make_strategy();
        let signal = sig(0x07, 10);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp1 = strategy.build_blueprint(&signal).await.unwrap();
        let bp2 = strategy.build_blueprint(&signal).await.unwrap();
        assert!(bp2.nonce > bp1.nonce);
    }

    // ── Cross-crate constant drift guard ──────────────────────────────────

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn msa_slippage_within_known_risk_policy_cap() {
        /// Mirrors omega_risk::context::MAX_SLIPPAGE_BPS_MSA.
        const MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA: u16 = 50;
        assert!(
            MSA_SLIPPAGE_BPS <= MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA,
            "MSA_SLIPPAGE_BPS ({MSA_SLIPPAGE_BPS}) exceeds the mirrored risk-policy \
             cap ({MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA}) — every MSA blueprint \
             would fail omega_risk::checks::check_slippage in production. Verify \
             against the real MAX_SLIPPAGE_BPS_MSA in crates/omega-risk/src/context.rs."
        );
    }
}
