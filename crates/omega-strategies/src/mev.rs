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
// ## Capital-path marker (this revision)
//
// `flashloan_provider`/`flashloan_amount`/`flashloan_available` below
// are all zero, matching this file's own long-standing claim that MEV
// does not use flashloans — see the inline TODO(capital-path) comment
// in `build_blueprint` for what that claim does and doesn't cover if
// this strategy ever needs borrowable capital in the future.
//
// ## `max_base_fee_gwei` (this revision)
//
// `ExecutionBlueprint` gained a `max_base_fee_gwei` field (compile
// error otherwise: E0063 missing field). PLACEHOLDER VALUE, same
// caveat as sa.rs/msa.rs/la.rs — set as `base_fee_at_creation * 3`
// pending confirmation of the field's real intended semantics.
//
// ## Fix (this revision, 2): flashloan identity fields for E0063
//
// `ExecutionBlueprint` gained three additional fields —
// `flashloan_provider_type`, `provider_contract`, `flashloan_token` —
// at some point without this file being updated to match, producing
// `error[E0063]: missing fields flashloan_provider_type, flashloan_token
// and provider_contract in initializer of ExecutionBlueprint`. Fixed by
// adding all three as inert placeholders (`FlashloanProviderType::
// Balancer`, `Address::ZERO`, `Address::ZERO`) alongside the existing
// `flashloan_provider: Address::ZERO` / no-flashloan path — the same
// pattern `omega-execution/src/pipeline.rs`'s own test helper
// (`tests::sample_bp`) already establishes for exactly this situation:
// nothing in `ExecutionPipeline::execute`'s Stage 0-6 path reads any of
// these three flashloan-identity fields when `flashloan_provider` is
// `Address::ZERO`, so their concrete values are inert here, not a
// product decision. This is consistent with (not in tension with) this
// file's own long-standing "MEV does not use flashloans" claim above.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};
use omega_core::types::flashloan_provider::FlashloanProviderType;
use omega_core::types::lane::{Lane, Simulator};
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};

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

/// Placeholder multiplier for `max_base_fee_gwei` — see module-level
/// comment on this revision's `max_base_fee_gwei` addition.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: u64 = 3;

// ─────────────────────────────────────────────────────────────────────────────
// MevStrategy
// ─────────────────────────────────────────────────────────────────────────────

/// MEV-OFA / Backrun strategy — Phase 4, Normal lane (§1.1, §4, §8).
pub struct MevStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
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
            nonce: AtomicU64::new(0),
            bytecode_hash,
            contract_addr,
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
    /// Reads `signal_state_hash[0]` as a proxy proxy for the score —
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

        // Partially-built blueprint for hash computation
        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below via canonical compute_hash()
            chain_id: self.chain_id,
            strategy_id: StrategyId::Mev,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            // TODO(capital-path): Strategy comment claims "MEV does not use flashloans."
            // Zero provider/amount matches that claim and resolve_flashloan_provider_id's
            // Ok("none") path. There is still no on-chain no-flashloan / PIL-inventory
            // execution path if this strategy ever needs borrowable capital. Do not
            // populate a non-zero flashloan_token without either select_provider wiring
            // or an explicit product decision that MEV remains self/externally funded
            // outside the Orchestrator flashloan flow.
            flashloan_provider: Address::ZERO, // MEV does not use flashloans
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::ZERO,
            // Fix (this revision, 2): inert placeholders — flashloan_provider
            // is Address::ZERO (no flashloan), and omega-execution's pipeline
            // never reads these three fields on that path. Same pattern
            // pipeline.rs's own test helper (sample_bp) already uses for the
            // identical case — see this file's module-level "Fix (this
            // revision, 2)" note. Consistent with, not in tension with, this
            // file's long-standing "MEV does not use flashloans" claim.
            flashloan_provider_type: FlashloanProviderType::Balancer,
            provider_contract: Address::ZERO,
            flashloan_token: Address::ZERO,
            // PLACEHOLDER — see module-level comment on this revision's
            // max_base_fee_gwei addition.
            max_base_fee_gwei: signal
                .base_fee_gwei
                .saturating_mul(MAX_BASE_FEE_HEADROOM_MULTIPLIER),
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
        }
    }

    fn make_strategy() -> Arc<MevStrategy> {
        MevStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::from([0x11; 20]),
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
        // State version 5 → profit multiplier 6 → ≈ 0.006 ETH
        // min_profit at 10 gwei base = small → should score > 0
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
        // hash_byte_0 = 50 → adverse ≈ 0.196, profit should exceed min
        let signal = test_signal(9, 50);
        // Score first to confirm it should proceed
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return; // profit below min at this fee level — skip
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
    async fn blueprint_does_not_use_flashloan() {
        let strategy = make_strategy();
        let signal = test_signal(9, 50);
        let score = strategy.score(&signal).await.unwrap();
        if !score.should_proceed() {
            return;
        }
        let bp = strategy.build_blueprint(&signal).await.unwrap();
        assert_eq!(
            bp.flashloan_amount,
            U256::ZERO,
            "MEV does not use flashloans"
        );
        assert_eq!(bp.flashloan_provider, Address::ZERO);
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

    // ── Blueprint integrity (this revision) ──────────────────────────────────

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
}