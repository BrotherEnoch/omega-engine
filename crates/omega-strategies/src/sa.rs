// crates/omega-strategies/src/sa.rs
//
// Simple Arbitrage (SA) — Phase 1 strategy (spec §1.1).
//
// ## Audit fix (this revision)
//
// `blueprint_hash` was previously computed from an ad hoc, strategy-local
// `keccak256(signal_state_hash || nonce || block_number)` — a much
// smaller field set than, and structurally different from,
// `ExecutionBlueprint::compute_hash()` (the canonical encoding defined in
// omega-core specifically so `verify_hash()` can catch post-construction
// mutation). Since the two never matched, `verify_hash()` would have
// reported every SA blueprint as tampered despite nothing being wrong.
// Fixed to build the blueprint with a placeholder hash, then call the
// canonical `bp.compute_hash()` — same pattern already used in
// `omega_core::types::blueprint`'s own tests.
//
// Also adds `signal_id`, `client_order_id`, `idempotency_key` — see
// `ExecutionBlueprint`'s own doc comments (omega-core) for what each is
// for. `idempotency_key` is filled in the same way, via
// `bp.compute_idempotency_key()`, after construction.
//
// ## Capital-path marker (this revision)
//
// `flashloan_provider`/`flashloan_amount` below are `Address::ZERO`/
// `U256::ZERO` — see the inline TODO(capital-path) comment in
// `build_blueprint` for the full status. Short version: this is a known,
// currently-unexecutable state being called out explicitly rather than
// left silent.
//
// ## `max_base_fee_gwei` (this revision)
//
// `ExecutionBlueprint` gained a `max_base_fee_gwei` field (compile
// error otherwise: E0063 missing field). PLACEHOLDER VALUE, same
// caveat as la.rs/msa.rs/mev.rs — set as `base_fee_at_creation * 3`
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
// product decision. See this file's own TODO(capital-path) comment
// below for the still-open question of what SA's real flashloan path
// (if any) should look like.

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
use omega_core::types::strategy::{OpScore, SignalState, SimResult};
use omega_core::{GasConfig, OmegaConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const SA_GAS_BUDGET: u64 = 200_000;
const SA_EXTRACTION_GAS: u64 = 21_000;
const SA_L1_DATA_GAS: u64 = 1_600;
const SA_EXPIRY_BLOCKS: u64 = 2;
const SA_SLIPPAGE_BPS: u16 = 50;
const SA_CONFIRMATION: u8 = 12;
const SA_SPREAD_WEI: u128 = 200_000_000_000_000_000; // 0.2 ETH

/// Placeholder multiplier for `max_base_fee_gwei` — see module-level
/// comment on this revision's `max_base_fee_gwei` addition.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: u64 = 3;

// ─────────────────────────────────────────────────────────────────────────────
// SaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct SaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    gas: GasConfig,
}

impl SaStrategy {
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
        let fee_pressure = signal.base_fee_gwei as f64 / 50.0;
        if fee_pressure > 1.0 {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 1.0,
            });
        }

        let spread_wei = U256::from(SA_SPREAD_WEI);
        match self.net_profit_after_gas(spread_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei) {
            None => Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.5,
            }),
            Some((net, _)) => {
                let competition_prob = 0.35_f64;
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
        let spread_wei = U256::from(SA_SPREAD_WEI);
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

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below via canonical compute_hash()
            chain_id: self.chain_id,
            strategy_id: StrategyId::Sa,
            lane: Lane::Microtx,
            simulator: Simulator::Revm,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            // TODO(capital-path): flashloan_provider == Address::ZERO is documented on
            // ExecutionBlueprint as "no flashloan — capital sourced from PIL (§7)", and
            // omega-execution maps zero → Ok("none") in resolve_flashloan_provider_id.
            // There is no Orchestrator branch and no strategy→PIL inventory path that
            // makes this executable on-chain (execute() reverts ZeroAddress on
            // flashloanToken == address(0); PilTreasury has deposit/redeem only, no
            // strategy loan/allocate). Either wire omega_flashloan::select_provider
            // (treat SA as incomplete flashloan strategy — Option B default) or
            // implement a real no-flashloan path as a product feature. Do not encode
            // or submit until one of those exists.
            flashloan_provider: Address::ZERO,
            flashloan_amount: U256::ZERO,
            flashloan_available: U256::MAX,
            // Fix (this revision, 2): inert placeholders — flashloan_provider
            // is Address::ZERO (no flashloan), and omega-execution's pipeline
            // never reads these three fields on that path. Same pattern
            // pipeline.rs's own test helper (sample_bp) already uses for the
            // identical case — see this file's module-level "Fix (this
            // revision, 2)" note.
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
            state_version: 1,
            chain_id: 42161,
            block_number: 1_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x01; 32]),
        }
    }

    #[test]
    fn strategy_metadata() {
        let s = make_strategy();
        assert_eq!(s.strategy_id(), StrategyId::Sa);
        assert_eq!(s.lane(), Lane::Microtx);
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
        let s = make_strategy();
        let op = s.score(&make_signal(100)).await.unwrap();
        assert_eq!(op.score, 0.0, "gas spike should suppress score");
    }

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
        // Fix E0061: encode_two_hop_calldata takes 4 args — contract_addr was missing
        let data = SaStrategy::encode_two_hop_calldata(
            Address::ZERO,
            U256::from(900_000_u64),
            U256::from(1_000_000_u64),
            1_000_100,
        );
        // selector(4) + amount_in(32) + min_out(32) + deadline(32) + addr(32) = 132 bytes
        assert_eq!(data.len(), 132);
    }

    // ── Blueprint integrity (this revision) ──────────────────────────────────

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        // Regression guard: previously blueprint_hash was computed via a
        // strategy-local ad hoc hash that never matched
        // ExecutionBlueprint::compute_hash(), so verify_hash() would have
        // failed for every SA blueprint. Now that build_blueprint calls
        // the canonical compute_hash(), this must pass.
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
}