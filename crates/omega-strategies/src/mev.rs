// crates/omega-strategies/src/mev.rs
//
// MEV-OFA / Backrun (MEV) — Phase 4 strategy (spec §1.1, §4, §8).
//
// ## Overview
//
//   MEV captures value from Order Flow Agreements (OFA): when a user
//   submits a swap through an OFA-compliant relay, MEV builds a backrun
//   bundle that trades after the user's swap in the same block, capturing
//   the resulting price impact as profit.
//
//   MEV is NOT front-running — the user's transaction executes first.
//   The MEV bundle references the user's tx hash and is submitted to
//   MEV-Share-compatible relays only.
//
// ## Spec constraints (§1.1, §4, §8)
//
//   Phase:        4 (MEV)
//   Lane:         Normal (Anvil simulation; calldata complex)
//   Simulator:    Anvil (full fork needed for post-swap state)
//   Gas budget:   300,000 L2 units
//   Hot-path:     false (OFA scoring is latency-tolerant vs LA)
//   OFA:          true — MUST route through OFA-compliant relays only
//   Min profit:   dynamic (from GasConfig + adverse selection discount)
//   Confirmation: 12 blocks
//
// ## Adverse selection guard (§8)
//
//   MEV includes an adverse selection detector: if the incoming order
//   flow signal has `adverse_selection_score > 0.5` the opportunity is
//   skipped (OpScore = 0.0).  High adverse-selection scores indicate
//   the OFA user's swap may be informed flow that will move the market
//   against the backrun position.
//
// ## Builder blacklist (§12.3)
//
//   MEV blueprints set `ofa_compliant = true`.  The relay layer filters
//   builders via the BuilderBlacklist before submission.
//
// ## Calldata encoding
//
//   The backrun calldata is a multi-hop swap through the pools affected
//   by the user's transaction, encoded as a strategy contract call.
//   `encode_calldata` returns `blueprint.calldata` directly — MEV does
//   not re-encode after simulation.
//
// ## Nonce
//
//   Per-chain AtomicU64, same scheme as SA and LA.
//
// ## Audit fix (this revision)
//
// This file previously had its own private `compute_blueprint_hash`
// associated function, hashing only (chain_id, signal_state_hash,
// state_version, nonce, l2_exec_gas_estimate, expected_profit_net) —
// structurally different from (and a strict subset of)
// `ExecutionBlueprint::compute_hash()`'s canonical encoding, and
// different again from every other strategy's own ad hoc hash (sa.rs,
// la.rs, msa.rs each had yet another bespoke variant, now all fixed the
// same way). `verify_hash()` would have failed for every MEV blueprint.
// Removed `compute_blueprint_hash` entirely and switched to the
// canonical `bp.compute_hash()`. Also adds `signal_id`,
// `client_order_id`, `idempotency_key`.
//
// ## Capital path (Option B) — this revision
//
// MEV previously set flashloan_provider/amount/token all to zero,
// which the engine's pre-submission check correctly rejects with
// "invalid flashloan identity: flashloan_token and amount are zero —
// Orchestrator has no no-flashloan execute path". Fixed by wiring
// `omega_flashloan::select_provider` with ARBITRUM_WETH as the asset,
// same as SA and MSA. The backrun notional (estimated_profit_wei) is
// used as the flashloan amount so the borrow size scales with the
// opportunity. `build_blueprint` fails closed if selection fails or the
// amount is zero — same posture as SA/MSA.
//
// ## `max_base_fee_gwei` — floor fix (this revision)
//
// `max_base_fee_gwei` now goes through
// `ExecutionBlueprint::derive_max_base_fee_gwei()` which floors the
// result at `MIN_MAX_BASE_FEE_GWEI` (1 gwei). On Arbitrum the real base
// fee (~0.02 gwei) truncates to 0 whole gwei, making the old inline
// `base_fee * 3` produce 0 — and a zero maxBaseFee makes
// OmegaOrchestrator.execute() revert with BaseFeeTooHigh on every call.
// Same fix already applied to sa.rs; now applied here.
// `MAX_BASE_FEE_HEADROOM_MULTIPLIER` is changed from `u64` to `f64`
// (3.0) to match `derive_max_base_fee_gwei`'s signature.

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

use crate::flashloan_select::to_blueprint_provider_type;

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const MEV_GAS_BUDGET: u64 = 300_000;
const MEV_EXTRACTION_GAS: u64 = 21_000;
/// Backrun calldata is typically 2–3 hops; ~250 bytes × 16.
const MEV_L1_DATA_GAS: u64 = 4_000;
/// MEV bundles must land in the current block to capture price impact.
const MEV_EXPIRY_BLOCKS: u64 = 1;
const MEV_SLIPPAGE_BPS: u16 = 30; // tighter than LA: backrun is arb-like
const MEV_CONFIRMATION: u8 = 12;

/// Adverse selection score threshold above which the opportunity is skipped.
///
/// Scores above 0.5 indicate the OFA user's swap is likely informed flow
/// that will move the market against the backrun position (§8).
const ADVERSE_SELECTION_THRESHOLD: f64 = 0.5;

/// Fraction of estimated price impact captured as MEV profit (30%).
///
/// The remaining 70% is competition buffer and slippage headroom.
const IMPACT_CAPTURE_FRACTION: f64 = 0.30;

/// Multiplier for `max_base_fee_gwei` — passed to
/// `ExecutionBlueprint::derive_max_base_fee_gwei()`, which also applies
/// the `MIN_MAX_BASE_FEE_GWEI` floor. Changed from u64 to f64 to match
/// that function's signature (same change already in sa.rs).
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

/// Canonical bridged WETH on Arbitrum One.
/// Must match Vault `profit_token` and `omega_rpc::WETH`.
const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

// ─────────────────────────────────────────────────────────────────────────────
// MevStrategy
// ─────────────────────────────────────────────────────────────────────────────

/// MEV-OFA / Backrun strategy — Phase 4, Normal lane (§1.1, §4, §8).
pub struct MevStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    gas: GasConfig,
}

impl MevStrategy {
    /// Construct from the engine config.
    ///
    /// `bytecode_hash` is the keccak256 of the deployed MEV contract's
    /// runtime bytecode, verified by the registry (§8).
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            liquidity_registry: LiquidityRegistry::new(),
            gas: config.gas.clone(),
        })
    }

    /// Construct with an explicit liquidity registry — used by main.rs
    /// so MEV shares the same registry handle as SA/MSA/LA.
    pub fn new_with_registry(
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

    // ── Gas model helpers ─────────────────────────────────────────────────

    /// Compute the dynamic minimum profit in wei using the dual-component
    /// gas model (§7).
    ///
    /// min_profit = (l2_gas × base_fee × l2_buf + l1_gas × l1_fee × l1_buf) × 1e9
    fn dynamic_min_profit(&self, signal: &SignalState) -> U256 {
        let l2_cost = MEV_GAS_BUDGET as f64
            * signal.base_fee_gwei as f64
            * self.gas.l2_buffer_factor
            * 1e9_f64;

        let l1_cost = MEV_L1_DATA_GAS as f64
            * signal.l1_data_fee_gwei as f64
            * self.gas.l1_data_buffer_factor
            * 1e9_f64;

        U256::from((l2_cost + l1_cost) as u128)
    }

    /// Extract the adverse selection score from the signal payload.
    ///
    /// Reads `signal_state_hash[0]` as a proxy for the score —
    /// in production the actual score is embedded in the OracleSignal
    /// payload by omega-oracle after parsing the MEV-Share SSE event.
    /// The hash byte gives a deterministic but realistic distribution
    /// for testing.
    fn adverse_selection_score(signal: &SignalState) -> f64 {
        signal.state_hash[0] as f64 / 255.0
    }

    /// Estimate the price impact captured as profit.
    ///
    /// In production this value is read from the OracleSignal payload
    /// (the decoded MEV-Share bundle's swap delta).  Here we derive a
    /// representative estimate from the signal version as a proxy.
    fn estimated_profit_wei(signal: &SignalState) -> U256 {
        // Proxy: 0.01–0.10 ETH depending on state version.
        // Real: parsed from OracleSignal::payload["price_impact_wei"]
        let base: u128 = 10_000_000_000_000_000; // 0.01 ETH
        let multiplier = (signal.state_version % 10 + 1) as u128;
        let gross_impact = base * multiplier;
        U256::from((gross_impact as f64 * IMPACT_CAPTURE_FRACTION) as u128)
    }

    /// Encode the backrun calldata for this signal state.
    ///
    /// In production: ABI-encodes the strategy contract call with the
    /// target pool addresses, swap amounts, and slippage bounds derived
    /// from the OracleSignal MEV-Share payload.
    ///
    /// Here: returns a deterministic 4-byte selector + chain_id for
    /// testability, matching the `blueprint.calldata` format.
    fn encode_calldata_for(signal: &SignalState, contract: Address) -> Bytes {
        let mut buf = Vec::with_capacity(36);
        // 4-byte selector: keccak256("backrun(uint64,bytes32)")[0..4]
        let selector = keccak256(b"backrun(uint64,bytes32)");
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
impl StrategyTrait for MevStrategy {
    // ── Static metadata ───────────────────────────────────────────────────

    fn strategy_id(&self) -> StrategyId {
        StrategyId::Mev
    }
    fn lane(&self) -> Lane {
        Lane::Normal
    }

    /// MEV is NOT hot-path eligible (§4).
    /// OFA scoring involves parsing MEV-Share payloads which is too slow
    /// for the <1ms Microtx hot path.
    fn hot_path_eligible(&self) -> bool {
        false
    }

    fn gas_budget(&self) -> u64 {
        MEV_GAS_BUDGET
    }

    fn base_min_profit_wei(&self) -> U256 {
        // 0.002 ETH baseline — must exceed competitive backrun costs
        U256::from(2_000_000_000_000_000_u128)
    }

    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    // ── score ─────────────────────────────────────────────────────────────

    /// Score an OFA backrun opportunity.
    ///
    /// Returns 0.0 when:
    ///   - The adverse selection score exceeds 0.5 (§8 guard)
    ///   - Estimated profit is below dynamic_min_profit
    ///
    /// Score formula:
    ///   raw_score = estimated_profit / (dynamic_min_profit × 2)
    ///   adjusted  = raw_score × (1 − competition_prob)
    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        // Adverse selection guard (§8)
        let adv_score = Self::adverse_selection_score(signal);
        if adv_score > ADVERSE_SELECTION_THRESHOLD {
            tracing::debug!(
                adverse_score = adv_score,
                threshold = ADVERSE_SELECTION_THRESHOLD,
                "MEV opportunity skipped: adverse selection too high",
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: adv_score,
            });
        }

        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = Self::estimated_profit_wei(signal);

        if est_profit <= min_profit {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: est_profit,
                competition_prob: adv_score,
            });
        }

        // Score: ratio of profit to 2× min threshold, discounted by adverse
        // selection probability (adverse score repurposed as competition proxy)
        let profit_f64 = est_profit.to_string().parse::<f64>().unwrap_or(0.0);
        let min_f64 = min_profit.to_string().parse::<f64>().unwrap_or(1.0);
        let raw_score = (profit_f64 / (min_f64 * 2.0)).min(1.0);
        let adj_score = (raw_score * (1.0 - adv_score)).clamp(0.0, 1.0);

        tracing::debug!(
            chain_id = signal.chain_id,
            state_version = signal.state_version,
            score = adj_score,
            adverse_score = adv_score,
            "MEV opportunity scored",
        );

        Ok(OpScore {
            score: adj_score,
            expected_profit: est_profit,
            competition_prob: adv_score,
        })
    }

    // ── build_blueprint ───────────────────────────────────────────────────

    /// Build a MEV backrun blueprint.
    ///
    /// The blueprint sets `ofa_compliant = true` — the relay layer will
    /// only submit this to MEV-Share-compatible relays that honour the
    /// builder blacklist (§12.3).
    async fn build_blueprint(&self, signal: &SignalState) -> Result<ExecutionBlueprint> {
        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let min_profit = self.dynamic_min_profit(signal);
        let est_profit = Self::estimated_profit_wei(signal);
        let calldata = Self::encode_calldata_for(signal, self.contract_addr);

        // Dual-component gas model (§7)
        let l2_buf = self.gas.l2_buffer_factor;
        let l1_buf = self.gas.l1_data_buffer_factor;

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Mev,
            self.chain_id,
            nonce,
            signal_id,
        );

        // Option B: real flashloan selection (fail closed).
        // MEV borrows WETH to fund the backrun swap; the estimated profit
        // determines the borrow size. Fails closed if the registry has no
        // liquidity yet — same posture as SA and MSA.
        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = est_profit;
        if flashloan_amount.is_zero() {
            anyhow::bail!("MEV: refusing zero flashloan_amount (estimated profit is zero)");
        }
        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("MEV: flashloan selection failed: {e:?}"))?;

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below via canonical compute_hash()
            chain_id: self.chain_id,
            strategy_id: StrategyId::Mev,
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
            // Floored at MIN_MAX_BASE_FEE_GWEI by derive_max_base_fee_gwei():
            // a sub-gwei Arbitrum base fee truncates to 0 gwei, and a zero
            // ceiling would revert every execute() with BaseFeeTooHigh.
            max_base_fee_gwei: ExecutionBlueprint::derive_max_base_fee_gwei(
                signal.base_fee_gwei,
                MAX_BASE_FEE_HEADROOM_MULTIPLIER,
            ),
            calldata,
            strategy_bytecode_hash: self.bytecode_hash,
            l2_exec_gas_estimate: MEV_GAS_BUDGET,
            l1_data_gas_estimate: MEV_L1_DATA_GAS,
            extraction_gas: MEV_EXTRACTION_GAS,
            expected_profit_net: est_profit,
            dynamic_min_profit: min_profit,
            l2_buffer_factor: l2_buf,
            l1_data_buffer_factor: l1_buf,
            slippage_bps: MEV_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: self.gas.max_priority_fee_gwei,
            price_impact_bps: Some(50), // 0.5% typical backrun impact
            ofa_compliant: true,        // MUST use OFA-compliant relays (§8)
            expiry_block: signal.block_number + MEV_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: MEV_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO, // filled below
            relay_targets: vec!["mev_share_primary".to_string()],
            zk_proof_commitment: None,
        };

        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();

        if !bp.is_profitable() {
            anyhow::bail!(
                "MEV blueprint unprofitable: profit={} < min={}",
                bp.expected_profit_net,
                bp.dynamic_min_profit,
            );
        }

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            nonce,
            expected_profit = %bp.expected_profit_net,
            priority_fee    = bp.priority_fee_gwei,
            "MEV blueprint built",
        );

        Ok(bp)
    }

    // ── simulate ──────────────────────────────────────────────────────────

    /// Simulate the MEV backrun using Anvil (full fork).
    ///
    /// Anvil is required because:
    ///   - The backrun accesses post-swap DEX pool state
    ///   - The simulation must replay the user's swap then our backrun
    ///   - revm does not support the MEV-Share tx ordering primitives
    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        // In production: spawn an Anvil fork, replay user tx then backrun tx,
        // read profit delta.  Here: return a representative result for the
        // gas estimate and profit fields already in the blueprint.
        let gas_used = (MEV_GAS_BUDGET as f64 * 0.78) as u64; // typical 78% utilisation
        let profit_net = bp.expected_profit_net;

        tracing::debug!(
            blueprint_hash = %bp.blueprint_hash,
            gas_used,
            profit_net     = %profit_net,
            simulator      = "anvil",
            "MEV simulation complete",
        );

        Ok(SimResult {
            profit_net,
            gas_used,
            simulator: "anvil".to_string(),
            success: true,
        })
    }

    // ── encode_calldata ───────────────────────────────────────────────────

    /// Return the calldata from the blueprint unchanged.
    ///
    /// MEV does not re-encode after simulation — the backrun calldata is
    /// fixed at blueprint construction time and the slippage bounds are
    /// set conservatively enough that simulation output doesn't change them.
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

    fn test_signal(state_version: u64, hash_byte_0: u8) -> SignalState {
        let mut hash = B256::ZERO;
        hash.0[0] = hash_byte_0;
        SignalState {
            state_version,
            chain_id: 42161,
            block_number: 1_000_000,
            base_fee_gwei: 10,
            l1_data_fee_gwei: 2,
            state_hash: hash,
            mev_share_events_in_window: 0,
        }
    }

    fn make_strategy() -> Arc<MevStrategy> {
        let liquidity_registry = LiquidityRegistry::new();
        liquidity_registry.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128), // 10 ETH available
            1,
        );
        MevStrategy::new_with_registry(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
            liquidity_registry,
            &OmegaConfig::default(),
        )
    }

    // ── Adverse selection guard ───────────────────────────────────────────

    #[tokio::test]
    async fn high_adverse_selection_skips() {
        let strategy = make_strategy();
        // hash_byte_0 = 255 → adverse_score = 1.0 > 0.5
        let signal = test_signal(1, 255);
        let score = strategy.score(&signal).await.unwrap();
        assert_eq!(
            score.score, 0.0,
            "high adverse selection must produce score 0"
        );
    }

    #[tokio::test]
    async fn low_adverse_selection_proceeds() {
        let strategy = make_strategy();
        // hash_byte_0 = 100 → adverse_score ≈ 0.39 < 0.5
        let signal = test_signal(5, 100);
        let score = strategy.score(&signal).await.unwrap();
        assert!(
            score.score > 0.0 || score.expected_profit == U256::ZERO,
            "low adverse selection should not be blocked: score={}",
            score.score
        );
    }

    // ── blueprint construction ────────────────────────────────────────────

    #[tokio::test]
    async fn blueprint_sets_ofa_compliant() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(
            bp.ofa_compliant,
            "MEV blueprints must set ofa_compliant=true"
        );
        assert_eq!(bp.strategy_id, StrategyId::Mev);
        assert_eq!(bp.simulator, Simulator::Anvil);
        assert!(
            !bp.blueprint_hash.is_zero(),
            "blueprint_hash must be non-zero"
        );
    }

    #[tokio::test]
    async fn blueprint_uses_flashloan() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert_ne!(
            bp.flashloan_amount,
            U256::ZERO,
            "MEV must now borrow via flashloan"
        );
        assert_ne!(bp.flashloan_provider, Address::ZERO);
        assert_ne!(bp.flashloan_token, Address::ZERO);
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
    }

    #[tokio::test]
    async fn build_blueprint_max_base_fee_is_never_zero_at_sub_gwei_base_fee() {
        let strategy = make_strategy();
        // base_fee_gwei = 0 simulates Arbitrum's real ~0.02 gwei truncating to 0
        let mut signal = test_signal(9, 50);
        signal.base_fee_gwei = 0;
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(
            bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI,
            "max_base_fee_gwei must never be below the floor, got {}",
            bp.max_base_fee_gwei
        );
        assert!(bp.max_base_fee_wei() > U256::ZERO);
        assert!(bp.verify_hash());
    }

    // ── simulation ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn simulate_returns_anvil_result() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        let res = strategy.simulate(&bp).await.unwrap();
        assert!(res.success);
        assert_eq!(res.simulator, "anvil");
        assert!(res.gas_used > 0 && res.gas_used <= MEV_GAS_BUDGET);
    }

    // ── static metadata ───────────────────────────────────────────────────

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Mev);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(!s.hot_path_eligible());
        assert!(!s.is_canary());
        assert_eq!(s.gas_budget(), MEV_GAS_BUDGET);
        assert_eq!(s.priority(), 0); // MEV has highest priority
    }

    // ── nonce monotonicity ────────────────────────────────────────────────

    #[tokio::test]
    async fn nonce_is_monotonically_increasing() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp1 = strategy.build_blueprint(&signal).await.unwrap();
        let bp2 = strategy.build_blueprint(&signal).await.unwrap();
        assert!(bp2.nonce > bp1.nonce, "nonces must be strictly increasing");
    }

    // ── Blueprint integrity ───────────────────────────────────────────────

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(
            bp.verify_hash(),
            "MEV blueprint must pass the canonical integrity check"
        );
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_idempotency_key() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_without_liquidity() {
        let empty = LiquidityRegistry::new();
        let s = MevStrategy::new_with_registry(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
            empty,
            &OmegaConfig::default(),
        );
        let signal = test_signal(9, 50);
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

    // ── Cross-crate constant drift guard ─────────────────────────────────────

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn mev_slippage_within_known_risk_policy_cap() {
        /// Mirrors omega_risk::context::MAX_SLIPPAGE_BPS_MEV.
        const MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MEV: u16 = 30;
        assert!(
            MEV_SLIPPAGE_BPS <= MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MEV,
            "MEV_SLIPPAGE_BPS ({MEV_SLIPPAGE_BPS}) exceeds the mirrored risk-policy \
             cap ({MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MEV}) — every MEV blueprint \
             would fail omega_risk::checks::check_slippage (check 9, MissSlippage) \
             in production. Verify against the real MAX_SLIPPAGE_BPS_MEV in \
             crates/omega-risk/src/context.rs before changing either value."
        );
    }
}
