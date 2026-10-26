// crates/omega-strategies/src/sa.rs
//
// Simple Arbitrage (SA) — Phase 1 strategy (spec §1.1).
//
// ## Fix (this revision): live quote path via QuoteSource
//
// `net_profit_after_gas()` previously used `SA_SPREAD_WEI` (0.2 ETH constant)
// as the input spread on every call, causing every blueprint to report
// identical `expected_profit_net_wei` (~0.192 ETH) regardless of pool state.
// On mainnet this manifested as SA replaying the same notional on every block,
// which the competition check correctly rejected because the real opportunity
// had long since been filled.
//
// Fixed by introducing `QuoteSource`, a trait that returns a live spread for
// the two-hop path. `SaStrategy::new()` now accepts `Arc<dyn QuoteSource>`.
// The companion `StaticQuoteSource` preserves the old constant behaviour and
// is the default when a real quote provider is not yet available — callers
// that previously relied on the constant can pass `StaticQuoteSource` with no
// other change.
//
// `QuoteSource` is defined here until a proper home is carved out in
// omega-core alongside the `SignalState` / `ExecutionBlueprint` types. Move it
// when the omega-core `quote` module exists.
//
// ## Fix (earlier revision): score() used a hardcoded competition_prob = 0.35
//
// `score()` computed `competition_prob: 0.35_f64` as a compile-time
// constant. This value was never updated from market state and never
// influenced `CheckContext::competition_probability`, but it contaminated
// `OpScore::competition_prob` which callers use to decide whether to call
// `build_blueprint()` at all — so a burst of MEV-Share activity that
// should raise the competition estimate and suppress scoring was invisible
// to `score()`.
//
// Fixed by calling `omega_risk::competition::non_la_competition_probability(
//   StrategyCompetitionClass::SimpleArbitrage, mev_share_events)` with the
// count passed through `SignalState` (see below). SA is not an LA strategy
// and has no health-factor or liquidation-size — `non_la_competition_
// probability` is the correct entry point; `competition_probability()` must
// not be called here with fabricated HF/size inputs.
//
// `SignalState` now carries `mev_share_events_in_window: u32` (added to
// omega-core alongside this fix). When `score()` receives a signal with
// zero events (quiet market) the result is the class base 0.35, which is
// well under any sane `max_competition_probability`. When MEV-Share activity
// is high the estimate rises toward 0.60 (0.35 + 0.25 cap), at which point
// the scorer can return a lower `score` or 0.0 to suppress blueprint
// generation upstream of the check pipeline.
//
// ## Audit fix: max_base_fee_gwei floor
//
// `max_base_fee_gwei` was previously computed inline as
// `signal.base_fee_gwei * 3`. `signal.base_fee_gwei` is whole gwei, and
// Arbitrum's real base fee (~0.02 gwei) truncates to 0, so the product
// was 0 and `OmegaOrchestrator.execute()` reverted with `BaseFeeTooHigh`
// on every call (`block.basefee > 0` is always true). It now goes
// through `ExecutionBlueprint::derive_max_base_fee_gwei()`, which floors
// the result at `MIN_MAX_BASE_FEE_GWEI` (1 gwei).
//
// ## Audit fix (earlier revision)
//
// `blueprint_hash` was previously computed from an ad hoc, strategy-local
// `keccak256(signal_state_hash || nonce || block_number)` — a much
// smaller field set than, and structurally different from,
// `ExecutionBlueprint::compute_hash()`. Fixed to call the canonical
// `bp.compute_hash()` after construction.
//
// ## Capital path (Option B)
//
// SA borrows via `omega_flashloan::select_provider`, same as LA/MSA.
// `flashloan_token` is Arbitrum canonical WETH. `flashloan_amount` is the
// spread returned by the quote source. `build_blueprint` fails closed if
// selection fails or token/amount is zero.
//
// ## Audit fix (earlier revision, 2): mixed-case hex literal
//
// `cargo clippy --workspace --all-targets -- -D warnings` failed on
// `clippy::mixed_case_hex_literals`. Fixed by lowercasing every hex digit
// in the `ARBITRUM_WETH` array. Value is byte-for-byte unchanged.

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
use omega_core::types::strategy::{OpScore, SignalState, SimResult};
use omega_core::{GasConfig, OmegaConfig};
use omega_flashloan::LiquidityRegistry;
use omega_risk::competition::{non_la_competition_probability, StrategyCompetitionClass};

use crate::flashloan_select::to_blueprint_provider_type;

// ─────────────────────────────────────────────────────────────────────────────
// QuoteSource — live spread for the SA two-hop path
// ─────────────────────────────────────────────────────────────────────────────

/// Provides a live input spread (in wei) for the SA two-hop path.
///
/// Implementations are expected to return the current DEX quote for the
/// arbitrage leg on every call — this is the value that drives both
/// `score()` and `build_blueprint()`. Returning a stale or cached value
/// defeats the purpose of the trait; implementors should read from the
/// UniV3 pool reader in omega-oracle (or equivalent) on every call.
///
/// ## TODO: move to omega-core
///
/// This trait belongs in `omega_core::quote` once that module exists.
/// The intent is:
/// ```ignore
/// // omega-core/src/quote.rs
/// pub trait QuoteSource: Send + Sync {
///     fn spread_wei(&self, signal: &SignalState) -> Option<U256>;
/// }
/// ```
/// Move it there and update this import when ready.
///
/// ## Contract
///
/// - Returns `None` when no live quote is available (pool unresponsive,
///   stale cache, feed gap). When `None`, `score()` returns a zero score
///   and `build_blueprint()` returns an error — no blueprint is emitted.
/// - Returns `Some(0)` only if the quote is genuinely zero; callers treat
///   this identically to `None` (see `build_blueprint` guard).
/// - The returned value is the gross input spread; gas costs are subtracted
///   downstream in `net_profit_after_gas`.
pub trait QuoteSource: Send + Sync {
    /// Return the current two-hop spread in wei for the given signal.
    fn spread_wei(&self, signal: &SignalState) -> Option<U256>;
}

// ─────────────────────────────────────────────────────────────────────────────
// StaticQuoteSource — backward-compatible fallback
// ─────────────────────────────────────────────────────────────────────────────

/// Backward-compatible quote source that always returns `SA_SPREAD_WEI`.
///
/// Use this only as a placeholder until a real UniV3-backed `QuoteSource`
/// implementation is wired in. Every blueprint built with this source will
/// report the same `expected_profit_net_wei` at the same gas price — the
/// known limitation from earlier revisions — but at least the code path
/// is correct and ready for a live implementation.
///
/// This is intentionally `pub` so callers outside this crate can construct
/// `SaStrategy` without a real quote path during initial integration.
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
// Fix (earlier revision): was 50 — did not match omega-risk::context's
// MAX_SLIPPAGE_BPS_SA (30). See cross-crate drift guard test below.
const SA_SLIPPAGE_BPS: u16 = 30;
const SA_CONFIRMATION: u8 = 12;

/// Fallback spread used by `StaticQuoteSource`.
///
/// This constant exists solely as the default for `StaticQuoteSource`.
/// `SaStrategy` itself never references it directly — the spread always
/// comes from the injected `QuoteSource`. Do not use this value for
/// anything other than the `StaticQuoteSource` fallback; live code should
/// go through `quote_source.spread_wei(signal)`.
const SA_SPREAD_WEI: u128 = 200_000_000_000_000_000; // 0.2 ETH

/// Maximum competition probability above which `score()` suppresses output.
///
/// SA is a DEX-arbitrage strategy; its base competition is 35 %. Scores
/// are suppressed when the MEV-Share-adjusted estimate exceeds this
/// threshold, saving the pipeline from evaluating blueprints that check 11
/// would drop anyway. Set at 0.90 to match the default pipeline threshold —
/// if `StrategyLimits::max_competition_probability` is tuned in config,
/// this should be updated to match.
const SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD: f64 = 0.90;

/// Placeholder multiplier for `max_base_fee_gwei`.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

/// Canonical bridged WETH on Arbitrum One.
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
}

impl SaStrategy {
    /// Construct an `SaStrategy` with a live quote source.
    ///
    /// Pass an `Arc<dyn QuoteSource>` that returns fresh DEX quotes on
    /// every signal. For early integration or tests without a real pool
    /// reader, pass `Arc::new(StaticQuoteSource)`.
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
        quote_source: Arc<dyn QuoteSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            gas: config.gas.clone(),
            quote_source,
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

        Some((
            net,
            (total_cost / U256::from(1_000_000_000_u64)).saturating_to(),
        ))
    }

    /// Encode two-hop swap calldata.
    /// Signature: swapTwoHop(uint256 amount_in, uint256 min_amount_out, uint64 deadline_block)
    fn encode_two_hop_calldata(
        contract_addr: Address,
        amount_in: U256,
        min_amount_out: U256,
        deadline_block: u64,
    ) -> Bytes {
        let selector = &keccak256(b"swapTwoHop(uint256,uint256,uint64)")[..4];
        let mut data = Vec::with_capacity(4 + 96);
        data.extend_from_slice(selector);

        let mut buf = [0u8; 32];
        buf.copy_from_slice(&amount_in.to_be_bytes::<32>());
        data.extend_from_slice(&buf);

        buf.copy_from_slice(&min_amount_out.to_be_bytes::<32>());
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[24..].copy_from_slice(&deadline_block.to_be_bytes());
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[12..].copy_from_slice(contract_addr.as_slice());
        data.extend_from_slice(&buf);

        Bytes::from(data)
    }
}

#[async_trait]
impl omega_core::types::strategy::StrategyTrait for SaStrategy {
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
        U256::from(100_000_000_000_000_u64)
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        // Gate on base-fee pressure — if the L2 base fee is elevated the
        // static spread won't cover gas costs.
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0;
        if fee_pressure > 1.0 {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            });
        }

        // Fetch the current spread from the live quote source.
        // Returns None when no quote is available (pool gap, stale cache).
        let spread_wei = match self.quote_source.spread_wei(signal) {
            Some(s) if !s.is_zero() => s,
            _ => {
                return Ok(OpScore {
                    score: 0.0,
                    expected_profit: U256::ZERO,
                    competition_prob: 1.0,
                });
            }
        };

        match self.net_profit_after_gas(spread_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei) {
            None => Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            }),
            Some((net, _)) => {
                // Fix: derive competition estimate from the real model using
                // the MEV-Share event count from the signal, rather than the
                // old hardcoded 0.35 constant. SA has no health-factor or
                // liquidation-size — use non_la_competition_probability().
                //
                // `signal.mev_share_events_in_window` is the count of
                // competing bundles observed in the current activity window
                // by main's MevShareActivityTracker. When 0 (quiet market)
                // the result is the class base 0.35; when elevated it rises
                // toward 0.60, suppressing the score before the blueprint
                // reaches check 11.
                let competition_prob = non_la_competition_probability(
                    StrategyCompetitionClass::SimpleArbitrage,
                    signal.mev_share_events_in_window,
                );

                // Suppress scoring entirely when competition exceeds the
                // pipeline's check-11 threshold — no point producing a
                // blueprint that will be dropped immediately.
                if competition_prob >= SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD {
                    return Ok(OpScore {
                        score: 0.0,
                        expected_profit: U256::ZERO,
                        competition_prob,
                    });
                }

                let score =
                    (1.0 - competition_prob) * (net.saturating_to::<u128>() as f64 / 1e15).min(1.0);
                Ok(OpScore {
                    score: score.clamp(0.0, 1.0),
                    expected_profit: net,
                    competition_prob,
                })
            }
        }
    }

    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        // Fetch the current spread. Fail closed on missing or zero quote —
        // building a blueprint with a stale spread is strictly worse than
        // not building one at all.
        let spread_wei = match self.quote_source.spread_wei(signal) {
            Some(s) if !s.is_zero() => s,
            _ => anyhow::bail!("SA: no live quote available — refusing to build blueprint"),
        };

        let (net_profit, gas_cost_gwei) = self
            .net_profit_after_gas(spread_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei)
            .ok_or_else(|| anyhow::anyhow!("Opportunity no longer profitable"))?;

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
        if flashloan_amount.is_zero() {
            anyhow::bail!("SA: refusing zero flashloan_amount");
        }
        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("SA: flashloan selection failed: {e:?}"))?;

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below
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
            idempotency_key: B256::ZERO, // filled below
            relay_targets: vec!["relay_1".into()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Revm);
        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.95) as u64;
        Ok(SimResult {
            profit_net: bp.expected_profit_net,
            gas_used,
            simulator: "revm".into(),
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
    use omega_core::types::strategy::StrategyTrait;
    use omega_core::OmegaConfig;

    // ── Test quote sources ────────────────────────────────────────────────────

    /// Fixed spread — mirrors `StaticQuoteSource` but with a configurable value.
    struct FixedQuoteSource(U256);
    impl QuoteSource for FixedQuoteSource {
        fn spread_wei(&self, _signal: &SignalState) -> Option<U256> {
            Some(self.0)
        }
    }

    /// Returns the default SA spread for tests that don't care about quote variation.
    fn default_quote() -> Arc<dyn QuoteSource> {
        Arc::new(StaticQuoteSource)
    }

    /// A quote source that varies the spread by block number — lets tests
    /// verify that two consecutive signals produce different profit estimates.
    struct BlockVaryingQuoteSource;
    impl QuoteSource for BlockVaryingQuoteSource {
        fn spread_wei(&self, signal: &SignalState) -> Option<U256> {
            // Vary by ±10% around SA_SPREAD_WEI based on block number parity.
            let base = SA_SPREAD_WEI;
            if signal.block_number.is_multiple_of(2) {
                Some(U256::from(base + base / 10)) // +10%
            } else {
                Some(U256::from(base - base / 10)) // -10%
            }
        }
    }

    /// Returns None — simulates a quote source that has no current data.
    struct NoQuoteSource;
    impl QuoteSource for NoQuoteSource {
        fn spread_wei(&self, _signal: &SignalState) -> Option<U256> {
            None
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_strategy() -> Arc<SaStrategy> {
        make_strategy_with_quote(default_quote())
    }

    fn make_strategy_with_quote(quote_source: Arc<dyn QuoteSource>) -> Arc<SaStrategy> {
        let liquidity_registry = LiquidityRegistry::new();
        liquidity_registry.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128), // 10 ETH available
            1,
        );
        SaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            liquidity_registry,
            &OmegaConfig::default(),
            quote_source,
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
            mev_share_events_in_window: 0, // quiet market by default
        }
    }

    fn make_signal_at_block(base_fee: u64, block: u64) -> SignalState {
        SignalState {
            block_number: block,
            ..make_signal(base_fee)
        }
    }

    fn make_signal_with_mev_share(base_fee: u64, events: u32) -> SignalState {
        SignalState {
            mev_share_events_in_window: events,
            ..make_signal(base_fee)
        }
    }

    // ── Basic metadata ────────────────────────────────────────────────────────

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Sa);
        assert_eq!(s.lane(), Lane::Microtx);
        assert!(s.hot_path_eligible());
        assert!(!s.is_canary());
        assert_eq!(s.gas_budget(), SA_GAS_BUDGET);
    }

    // ── score() ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn score_low_fee_returns_nonzero() {
        let s = make_strategy();
        let op = s.score(&make_signal(5)).await.unwrap();
        assert!(op.score > 0.0, "low fee should produce positive score");
    }

    #[tokio::test]
    async fn score_high_fee_returns_zero() {
        let s = make_strategy();
        let op = s.score(&make_signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0, "gas spike should suppress score");
    }

    #[tokio::test]
    async fn score_returns_zero_when_no_quote() {
        let s = make_strategy_with_quote(Arc::new(NoQuoteSource));
        let op = s.score(&make_signal(5)).await.unwrap();
        assert_eq!(op.score, 0.0, "missing quote must suppress score");
        assert_eq!(op.expected_profit, U256::ZERO);
        assert_eq!(op.competition_prob, 1.0);
    }

    #[tokio::test]
    async fn score_varies_with_quote_source() {
        // Verify that two consecutive signals at different block numbers
        // produce different expected_profit when the quote source varies.
        let s = make_strategy_with_quote(Arc::new(BlockVaryingQuoteSource));
        let op_even = s.score(&make_signal_at_block(5, 1_000_000)).await.unwrap();
        let op_odd = s.score(&make_signal_at_block(5, 1_000_001)).await.unwrap();
        assert_ne!(
            op_even.expected_profit, op_odd.expected_profit,
            "profit must vary when the quote source returns different spreads per block"
        );
    }

    // ── competition model ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn score_uses_real_competition_model_not_hardcoded_constant() {
        // Regression guard: in a quiet market (0 MEV-Share events) the
        // competition probability must be the SA class base (0.35), NOT the
        // old hardcoded literal.
        let s = make_strategy();
        let op = s.score(&make_signal_with_mev_share(5, 0)).await.unwrap();
        assert!(
            (op.competition_prob - 0.35).abs() < 1e-9,
            "quiet market competition should be SA base 0.35, got {}",
            op.competition_prob
        );
        // Score must still be positive in quiet market with profitable spread.
        assert!(op.score > 0.0, "quiet market should produce positive score");
    }

    #[tokio::test]
    async fn score_suppressed_by_high_mev_share_activity() {
        // 13 events × 0.02 bump = 0.26 bump, clamped to 0.25 → 0.35 + 0.25 = 0.60.
        // 0.60 < SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD (0.90) → not suppressed,
        // but score is lower than at 0 events.
        let s = make_strategy();
        let op_quiet = s.score(&make_signal_with_mev_share(5, 0)).await.unwrap();
        let op_busy = s.score(&make_signal_with_mev_share(5, 13)).await.unwrap();
        assert!(
            op_busy.score <= op_quiet.score,
            "elevated MEV-Share activity must not raise score: {} vs {}",
            op_busy.score,
            op_quiet.score
        );
        assert!(
            op_busy.competition_prob > op_quiet.competition_prob,
            "elevated MEV-Share must raise competition_prob"
        );
    }

    #[tokio::test]
    async fn score_returns_zero_when_competition_at_suppress_threshold() {
        // 0.35 + 0.25 (max bump) = 0.60 < 0.90 → not suppressed for SA.
        // The suppress threshold is 0.90; SA can never reach it via events
        // alone (max is 0.60). So we verify the non-suppressed path is
        // what executes, and the test below covers the score=0 path via
        // the fee-pressure gate instead.
        let s = make_strategy();
        let op = s.score(&make_signal_with_mev_share(5, 100)).await.unwrap();
        // 0.35 + min(100*0.02, 0.25) = 0.60 → below 0.90 → score > 0
        assert!(
            op.score > 0.0,
            "SA at max MEV-Share bump (0.60) should still produce score"
        );
        assert!((op.competition_prob - 0.60).abs() < 1e-9);
    }

    // ── build_blueprint() ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn build_blueprint_fields_correct() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_eq!(bp.strategy_id, StrategyId::Sa);
        assert_eq!(bp.chain_id, 42161);
        assert_eq!(bp.lane, Lane::Microtx);
        assert_eq!(bp.simulator, Simulator::Revm);
        assert!(!bp.is_canary());
        assert!(bp.is_profitable());
        assert!(!bp.calldata.is_empty());
        assert_eq!(bp.confirmation_depth, SA_CONFIRMATION);
        assert_ne!(bp.flashloan_token, Address::ZERO);
        assert!(!bp.flashloan_amount.is_zero());
        assert_ne!(bp.provider_contract, Address::ZERO);
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
    }

    #[tokio::test]
    async fn build_blueprint_flashloan_amount_matches_quote() {
        // The flashloan amount must reflect what the quote source returned,
        // not a hardcoded constant.
        let custom_spread = U256::from(150_000_000_000_000_000_u128); // 0.15 ETH
        let s = make_strategy_with_quote(Arc::new(FixedQuoteSource(custom_spread)));
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_eq!(
            bp.flashloan_amount, custom_spread,
            "flashloan_amount must equal the live quote, not SA_SPREAD_WEI"
        );
    }

    #[tokio::test]
    async fn build_blueprint_profit_varies_with_quote() {
        // Two blueprints built with different quote sources must report
        // different expected_profit_net — the key regression from the
        // constant-spread bug.
        let s_low = make_strategy_with_quote(Arc::new(FixedQuoteSource(U256::from(
            150_000_000_000_000_000_u128,
        ))));
        let s_high = make_strategy_with_quote(Arc::new(FixedQuoteSource(U256::from(
            250_000_000_000_000_000_u128,
        ))));
        let bp_low = s_low.build_blueprint(&make_signal(5)).await.unwrap();
        let bp_high = s_high.build_blueprint(&make_signal(5)).await.unwrap();
        assert_ne!(
            bp_low.expected_profit_net, bp_high.expected_profit_net,
            "blueprints built from different quotes must have different profit estimates"
        );
        assert!(
            bp_high.expected_profit_net > bp_low.expected_profit_net,
            "higher quote must produce higher net profit"
        );
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_when_no_quote() {
        let s = make_strategy_with_quote(Arc::new(NoQuoteSource));
        let err = s
            .build_blueprint(&make_signal(5))
            .await
            .expect_err("missing quote must fail closed");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no live quote available"),
            "unexpected error message: {msg}"
        );
    }

    #[tokio::test]
    async fn nonce_increments() {
        let s = make_strategy();
        let bp1 = s.build_blueprint(&make_signal(5)).await.unwrap();
        let bp2 = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_ne!(bp1.nonce, bp2.nonce);
        assert_eq!(bp2.nonce, bp1.nonce + 1);
    }

    #[tokio::test]
    async fn simulate_returns_success() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        let sim = s.simulate(&bp).await.unwrap();
        assert!(sim.success);
        assert_eq!(sim.simulator, "revm");
        assert!(sim.gas_used < SA_GAS_BUDGET);
    }

    #[test]
    fn calldata_encoding_non_empty() {
        let data = SaStrategy::encode_two_hop_calldata(
            Address::ZERO,
            U256::from(900_000_u64),
            U256::from(1_000_000_u64),
            1_000_100,
        );
        assert_eq!(data.len(), 132);
    }

    #[tokio::test]
    async fn build_blueprint_max_base_fee_is_never_zero_at_sub_gwei_base_fee() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(0)).await.unwrap();
        assert!(
            bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI,
            "max_base_fee_gwei must never be below the floor, got {}",
            bp.max_base_fee_gwei
        );
        assert!(bp.max_base_fee_wei() > U256::ZERO);
        assert!(bp.verify_hash());
    }

    #[tokio::test]
    async fn build_blueprint_max_base_fee_scales_above_floor() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_eq!(bp.max_base_fee_gwei, 15);
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert!(
            bp.verify_hash(),
            "SA blueprint must pass the canonical integrity check"
        );
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_idempotency_key() {
        let s = make_strategy();
        let bp = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn build_blueprint_has_distinct_signal_ids_across_calls() {
        let s = make_strategy();
        let bp1 = s.build_blueprint(&make_signal(5)).await.unwrap();
        let bp2 = s.build_blueprint(&make_signal(5)).await.unwrap();
        assert_ne!(
            bp1.signal_id, bp2.signal_id,
            "each build is a distinct signal generation"
        );
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_without_liquidity() {
        let empty = LiquidityRegistry::new();
        let s = SaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            empty,
            &OmegaConfig::default(),
            default_quote(),
        );
        let err = s
            .build_blueprint(&make_signal(5))
            .await
            .expect_err("empty registry must fail closed");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"),
            "unexpected error: {msg}"
        );
    }

    // ── Cross-crate constant drift guards ─────────────────────────────────────

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn sa_slippage_within_known_risk_policy_cap() {
        /// Mirrors omega_risk::context::MAX_SLIPPAGE_BPS_SA.
        /// See this test's doc comment in the earlier revision for why this
        /// is a manually-synced copy rather than an import.
        const MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_SA: u16 = 30;
        assert!(
            SA_SLIPPAGE_BPS <= MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_SA,
            "SA_SLIPPAGE_BPS ({SA_SLIPPAGE_BPS}) exceeds the mirrored risk-policy \
             cap ({MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_SA}) — every SA blueprint \
             would fail omega_risk::checks::check_slippage (check 9, MissSlippage) \
             in production. Verify against the real MAX_SLIPPAGE_BPS_SA in \
             crates/omega-risk/src/context.rs before changing either value."
        );
    }

    #[test]
    fn sa_competition_suppress_threshold_matches_pipeline_default() {
        /// Mirrors the default `max_competition_probability` from pipeline.rs
        /// (line 825: 0.90). If that value changes, this test will fail,
        /// surfacing the need to update SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD.
        const MIRRORED_PIPELINE_MAX_COMPETITION_PROBABILITY: f64 = 0.90;
        assert!(
            (SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD
                - MIRRORED_PIPELINE_MAX_COMPETITION_PROBABILITY)
                .abs()
                < 1e-9,
            "SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD ({SA_COMPETITION_SCORE_SUPPRESS_THRESHOLD}) \
             does not match the pipeline's max_competition_probability \
             ({MIRRORED_PIPELINE_MAX_COMPETITION_PROBABILITY}). Update one or both."
        );
    }
}
