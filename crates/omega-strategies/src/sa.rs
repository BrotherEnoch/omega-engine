// crates/omega-strategies/src/sa.rs
//
// Simple Arbitrage (SA) — Phase 1 strategy (spec §1.1).
//
// ## QuoteSource (earlier revision)
//
// `net_profit_after_gas()` previously used `SA_SPREAD_WEI` (0.2 ETH constant)
// on every call. Fixed by introducing `QuoteSource`, a trait that returns a
// live spread for the two-hop path. `StaticQuoteSource` preserves the old
// constant behaviour as a fallback for callers not yet wired to a real pool
// reader.
//
// ## Competition model (earlier revision)
//
// `score()` previously hardcoded `competition_prob = 0.35`. Fixed to call
// `omega_risk::competition::non_la_competition_probability()`. `SignalState`
// no longer carries `mev_share_events_in_window` so the event count is passed
// as 0 (quiet-market base = 0.35) until a replacement side-channel is wired in.
//
// ## max_base_fee_gwei floor (earlier revision)
//
// Fixed via `ExecutionBlueprint::derive_max_base_fee_gwei()` which floors at
// `MIN_MAX_BASE_FEE_GWEI` (1 gwei). Arbitrum's real base fee truncates to
// 0 whole gwei, which made every execute() revert with BaseFeeTooHigh.
//
// ## SimulatorBackend injection (this revision)
//
// `simulate()` previously returned `success: true` unconditionally with a
// fabricated `gas_used = estimate * 0.95`. Fixed by accepting an injectable
// `SimulatorBackend`. When Some, the real revm backend executes the blueprint
// and returns a verified SimResult. When None, `simulate()` returns an error
// — no blueprint reaches the relay without actual simulation.

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
use omega_risk::competition::{non_la_competition_probability, StrategyCompetitionClass};

use crate::flashloan_select::to_blueprint_provider_type;
use crate::simulator::SimulatorBackend;

// ─────────────────────────────────────────────────────────────────────────────
// QuoteSource
// ─────────────────────────────────────────────────────────────────────────────

/// Provides a live input spread (in wei) for the SA two-hop path.
///
/// Implementations read from the UniV3 pool reader in omega-oracle on every
/// call. Returning stale or cached values defeats the purpose of this trait.
///
/// Returns `None` when no live quote is available. When `None`, `score()`
/// returns zero and `build_blueprint()` fails closed.
pub trait QuoteSource: Send + Sync {
    fn spread_wei(&self, signal: &SignalState) -> Option<U256>;
}

/// Backward-compatible fallback that always returns `SA_SPREAD_WEI`.
/// Use only as a placeholder until a real UniV3-backed implementation is wired in.
pub struct StaticQuoteSource;

impl QuoteSource for StaticQuoteSource {
    fn spread_wei(&self, _signal: &SignalState) -> Option<U256> {
        Some(U256::from(SA_SPREAD_WEI))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const SA_GAS_BUDGET: u64 = 200_000;
const SA_EXTRACTION_GAS: u64 = 21_000;
const SA_L1_DATA_GAS: u64 = 1_600;
const SA_EXPIRY_BLOCKS: u64 = 2;
const SA_SLIPPAGE_BPS: u16 = 30;
const SA_CONFIRMATION: u8 = 12;

/// Fallback spread used only by `StaticQuoteSource`. Never referenced by
/// `SaStrategy` directly — always comes from the injected `QuoteSource`.
const SA_SPREAD_WEI: u128 = 200_000_000_000_000_000; // 0.2 ETH

/// Competition probability above which `score()` suppresses output.
/// Matches the pipeline's default `max_competition_probability` (0.90).
const SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD: f64 = 0.90;

const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

/// Canonical bridged WETH on Arbitrum One.
/// Must match Vault `profit_token` and `omega_rpc::WETH`.
const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

// ─────────────────────────────────────────────────────────────────────────────
// SaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct SaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    gas: GasConfig,
    quote_source: Arc<dyn QuoteSource>,
    /// Real revm simulator. When None, `simulate()` returns an error.
    simulator: Option<Arc<dyn SimulatorBackend>>,
}

impl SaStrategy {
    /// Construct an `SaStrategy`.
    ///
    /// - `quote_source`: live DEX quotes. Pass `Arc::new(StaticQuoteSource)`
    ///   only during integration before a real pool reader is wired.
    /// - `simulator`: revm backend from `omega-simulation`. Pass `None` only
    ///   in tests that do not exercise `simulate()`.
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
        quote_source: Arc<dyn QuoteSource>,
        simulator: Option<Arc<dyn SimulatorBackend>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            gas: config.gas.clone(),
            quote_source,
            simulator,
        })
    }

    fn net_profit_after_gas(
        &self,
        spread_wei: U256,
        base_fee: u64,
        l1_data_fee: u64,
    ) -> Option<(U256, u64)> {
        let l2_fee_total_gwei = base_fee.saturating_add(
            (self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction) as u64,
        );

        let l2_cost_wei = U256::from((SA_GAS_BUDGET as f64 * self.gas.l2_buffer_factor) as u64)
            .saturating_mul(U256::from(l2_fee_total_gwei))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let l1_cost_wei =
            U256::from((SA_L1_DATA_GAS as f64 * self.gas.l1_data_buffer_factor) as u64)
                .saturating_mul(U256::from(l1_data_fee))
                .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost = l2_cost_wei.saturating_add(l1_cost_wei);
        if spread_wei <= total_cost {
            return None;
        }

        let net = spread_wei.saturating_sub(total_cost);
        let dynamic = U256::from(base_fee)
            .saturating_mul(U256::from(SA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));
        if net <= dynamic {
            return None;
        }

        let priority_gwei: u64 = u128::try_from(total_cost / U256::from(1_000_000_000_u64))
            .unwrap_or(u64::MAX as u128) as u64;
        Some((net, priority_gwei))
    }

    /// Encode two-hop swap calldata.
    /// Selector: `swapTwoHop(uint256 amountIn, uint256 minAmountOut, uint64 deadlineBlock)`
    ///
    /// IMPORTANT: verify this selector against the deployed SimpleArb.sol ABI.
    /// Note: the contract address is appended after the three ABI args for
    /// on-chain self-verification; confirm the Solidity function accepts this
    /// layout (or strip the trailing word if the contract does not).
    fn encode_two_hop_calldata(
        contract_addr: Address,
        amount_in: U256,
        min_amount_out: U256,
        deadline_block: u64,
    ) -> Bytes {
        let selector = &keccak256(b"swapTwoHop(uint256,uint256,uint64)")[..4];
        let mut data = Vec::with_capacity(4 + 128);
        data.extend_from_slice(selector);

        data.extend_from_slice(&amount_in.to_be_bytes::<32>());
        data.extend_from_slice(&min_amount_out.to_be_bytes::<32>());

        // uint64 deadline packed into a 32-byte word (right-aligned)
        let mut word = [0u8; 32];
        word[24..].copy_from_slice(&deadline_block.to_be_bytes());
        data.extend_from_slice(&word);

        // contract address (passed for on-chain self-verification)
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(contract_addr.as_slice());
        data.extend_from_slice(&word);

        Bytes::from(data)
    }
}

#[async_trait]
impl StrategyTrait for SaStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::Sa
    }

    fn lane(&self) -> Lane {
        Lane::Microtx
    }

    fn hot_path_eligible(&self) -> bool {
        true
    }

    fn gas_budget(&self) -> u64 {
        SA_GAS_BUDGET
    }

    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    fn base_min_profit_wei(&self) -> U256 {
        U256::from(100_000_000_000_000_u64) // 0.0001 ETH
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0;
        if fee_pressure > 1.0 {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            });
        }

        let spread_wei = match self.quote_source.spread_wei(signal) {
            Some(s) if !s.is_zero() => s,
            _ => {
                return Ok(OpScore {
                    score: 0.0,
                    expected_profit: U256::ZERO,
                    competition_prob: 1.0,
                })
            }
        };

        match self.net_profit_after_gas(spread_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei) {
            None => Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            }),
            Some((net, _)) => {
                // SignalState no longer carries mev_share_events_in_window;
                // pass 0 (base rate 0.35) until a replacement is wired in.
                let competition_prob =
                    non_la_competition_probability(StrategyCompetitionClass::SimpleArbitrage, 0);
                if competition_prob >= SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD {
                    return Ok(OpScore {
                        score: 0.0,
                        expected_profit: U256::ZERO,
                        competition_prob,
                    });
                }
                let net_f = u128::try_from(net).unwrap_or(u128::MAX) as f64;
                let score = (1.0 - competition_prob) * (net_f / 1e15).min(1.0);
                Ok(OpScore {
                    score: score.clamp(0.0, 1.0),
                    expected_profit: net,
                    competition_prob,
                })
            }
        }
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let spread_wei = match self.quote_source.spread_wei(signal) {
            Some(s) if !s.is_zero() => s,
            _ => anyhow::bail!("SA: no live quote available — refusing to build blueprint"),
        };

        let (net_profit, gas_cost_gwei) = self
            .net_profit_after_gas(spread_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei)
            .ok_or_else(|| anyhow::anyhow!("SA: opportunity no longer profitable"))?;

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let calldata = Self::encode_two_hop_calldata(
            self.contract_addr,
            spread_wei,
            net_profit,
            signal.block_number + SA_EXPIRY_BLOCKS,
        );

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Sa,
            self.chain_id,
            nonce,
            signal_id,
        );

        let dynamic_min = U256::from(signal.base_fee_gwei)
            .saturating_mul(U256::from(SA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = spread_wei;
        anyhow::ensure!(
            !flashloan_amount.is_zero(),
            "SA: refusing zero flashloan_amount"
        );

        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("SA: flashloan selection failed: {e:?}"))?;

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO,
            chain_id: self.chain_id,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
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
            l2_exec_gas_estimate: SA_GAS_BUDGET,
            l1_data_gas_estimate: SA_L1_DATA_GAS,
            extraction_gas: SA_EXTRACTION_GAS,
            expected_profit_net: net_profit,
            dynamic_min_profit: dynamic_min,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: SA_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: gas_cost_gwei.min(self.gas.max_priority_fee_gwei),
            price_impact_bps: Some(30),
            ofa_compliant: false,
            expiry_block: signal.block_number + SA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: SA_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["relay_1".into()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Revm);
        match &self.simulator {
            Some(backend) => backend.simulate(bp).await,
            None => anyhow::bail!(
                "SA: no SimulatorBackend injected — cannot simulate blueprint {}; \
                 pass a real revm backend via SaStrategy::new()",
                bp.blueprint_hash
            ),
        }
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

    struct FixedQuoteSource(U256);
    impl QuoteSource for FixedQuoteSource {
        fn spread_wei(&self, _: &SignalState) -> Option<U256> {
            Some(self.0)
        }
    }

    struct BlockVaryingQuoteSource;
    impl QuoteSource for BlockVaryingQuoteSource {
        fn spread_wei(&self, signal: &SignalState) -> Option<U256> {
            let base = SA_SPREAD_WEI;
            if signal.block_number.is_multiple_of(2) {
                Some(U256::from(base + base / 10))
            } else {
                Some(U256::from(base - base / 10))
            }
        }
    }

    struct NoQuoteSource;
    impl QuoteSource for NoQuoteSource {
        fn spread_wei(&self, _: &SignalState) -> Option<U256> {
            None
        }
    }

    fn default_quote() -> Arc<dyn QuoteSource> {
        Arc::new(StaticQuoteSource)
    }

    fn make_strategy() -> Arc<SaStrategy> {
        make_strategy_with_quote(default_quote())
    }

    fn make_strategy_with_quote(q: Arc<dyn QuoteSource>) -> Arc<SaStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128),
            1,
        );
        SaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            lr,
            &OmegaConfig::default(),
            q,
            None,
        )
    }

    fn make_signal(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 1,
            chain_id: 42161,
            block_number: 1_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x01; 32]),
        }
    }

    fn make_signal_at_block(base_fee: u64, block: u64) -> SignalState {
        SignalState {
            block_number: block,
            ..make_signal(base_fee)
        }
    }

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Sa);
        assert_eq!(s.lane(), Lane::Microtx);
        assert!(s.hot_path_eligible());
        assert_eq!(s.gas_budget(), SA_GAS_BUDGET);
    }

    #[tokio::test]
    async fn score_low_fee_returns_nonzero() {
        let op = make_strategy().score(&make_signal(5)).await.unwrap();
        assert!(op.score > 0.0);
    }

    #[tokio::test]
    async fn score_high_fee_returns_zero() {
        let op = make_strategy().score(&make_signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn score_zero_when_no_quote() {
        let s = make_strategy_with_quote(Arc::new(NoQuoteSource));
        let op = s.score(&make_signal(5)).await.unwrap();
        assert_eq!(op.score, 0.0);
        assert_eq!(op.competition_prob, 1.0);
    }

    #[tokio::test]
    async fn score_varies_with_quote_source() {
        let s = make_strategy_with_quote(Arc::new(BlockVaryingQuoteSource));
        let op_even = s.score(&make_signal_at_block(5, 1_000_000)).await.unwrap();
        let op_odd = s.score(&make_signal_at_block(5, 1_000_001)).await.unwrap();
        assert_ne!(op_even.expected_profit, op_odd.expected_profit);
    }

    #[tokio::test]
    async fn score_uses_base_rate_competition_model() {
        let op = make_strategy().score(&make_signal(5)).await.unwrap();
        assert!(
            (op.competition_prob - 0.35).abs() < 1e-9,
            "expected base rate 0.35, got {}",
            op.competition_prob
        );
        assert!(op.score > 0.0);
    }

    #[tokio::test]
    async fn build_blueprint_fields_correct() {
        let bp = make_strategy()
            .build_blueprint(&make_signal(5))
            .await
            .unwrap();
        assert_eq!(bp.strategy_id, StrategyId::Sa);
        assert_eq!(bp.lane, Lane::Microtx);
        assert_eq!(bp.simulator, Simulator::Revm);
        assert!(bp.is_profitable());
        assert!(!bp.calldata.is_empty());
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
        assert!(!bp.flashloan_amount.is_zero());
    }

    #[tokio::test]
    async fn build_blueprint_flashloan_amount_matches_quote() {
        let spread = U256::from(150_000_000_000_000_000_u128);
        let s = make_strategy_with_quote(Arc::new(FixedQuoteSource(spread)));
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_eq!(bp.flashloan_amount, spread);
    }

    #[tokio::test]
    async fn build_blueprint_profit_varies_with_quote() {
        let s_lo = make_strategy_with_quote(Arc::new(FixedQuoteSource(U256::from(
            150_000_000_000_000_000_u128,
        ))));
        let s_hi = make_strategy_with_quote(Arc::new(FixedQuoteSource(U256::from(
            250_000_000_000_000_000_u128,
        ))));
        let bp_lo = s_lo.build_blueprint(&make_signal(5)).await.unwrap();
        let bp_hi = s_hi.build_blueprint(&make_signal(5)).await.unwrap();
        assert!(bp_hi.expected_profit_net > bp_lo.expected_profit_net);
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_no_quote() {
        let s = make_strategy_with_quote(Arc::new(NoQuoteSource));
        let err = s
            .build_blueprint(&make_signal(5))
            .await
            .expect_err("must fail");
        assert!(format!("{err:?}").contains("no live quote"));
    }

    #[tokio::test]
    async fn simulate_fails_without_backend() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        let err = s
            .simulate(&bp)
            .await
            .expect_err("must fail without backend");
        assert!(format!("{err:?}").contains("no SimulatorBackend"));
    }

    #[tokio::test]
    async fn nonce_increments() {
        let s = make_strategy();
        let bp1 = s.build_blueprint(&make_signal(5)).await.unwrap();
        let bp2 = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_eq!(bp2.nonce, bp1.nonce + 1);
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        let bp = make_strategy()
            .build_blueprint(&make_signal(5))
            .await
            .unwrap();
        assert!(bp.verify_hash());
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_idempotency_key() {
        let bp = make_strategy()
            .build_blueprint(&make_signal(5))
            .await
            .unwrap();
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn max_base_fee_never_zero_at_sub_gwei() {
        let bp = make_strategy()
            .build_blueprint(&make_signal(0))
            .await
            .unwrap();
        assert!(bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI);
        assert!(bp.max_base_fee_wei() > U256::ZERO);
        assert!(bp.verify_hash());
    }

    #[tokio::test]
    async fn max_base_fee_scales_above_floor() {
        let bp = make_strategy()
            .build_blueprint(&make_signal(5))
            .await
            .unwrap();
        assert_eq!(bp.max_base_fee_gwei, 15); // 5 * 3.0
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_without_liquidity() {
        let s = SaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            LiquidityRegistry::new(),
            &OmegaConfig::default(),
            default_quote(),
            None,
        );
        let err = s
            .build_blueprint(&make_signal(5))
            .await
            .expect_err("must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"));
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn sa_slippage_within_known_risk_policy_cap() {
        const MIRRORED: u16 = 30;
        assert!(
            SA_SLIPPAGE_BPS <= MIRRORED,
            "SA_SLIPPAGE_BPS ({SA_SLIPPAGE_BPS}) exceeds mirrored risk cap ({MIRRORED})"
        );
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn sa_competition_suppress_threshold_matches_pipeline_default() {
        const MIRRORED: f64 = 0.90;
        assert!(
            (SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD - MIRRORED).abs() < 1e-9,
            "SA threshold {SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD} != pipeline default {MIRRORED}"
        );
    }
}
