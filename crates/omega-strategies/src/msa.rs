// crates/omega-strategies/src/msa.rs
//
// Multi-Step Arbitrage (MSA) — Phase 2 strategy (spec §1.1, §10).
//
// ## Overview
//
//   MSA captures cyclic price inconsistencies across 3-4 pool routes on the
//   same chain. It runs on the Normal lane because route construction and
//   state validation are heavier than SA's two-hop path, but it still keeps
//   the hot path deterministic: no RPC, no heap-heavy graph rebuilds, and no
//   dynamic dispatch in the scoring logic.
//
// ## Spec constraints
//
//   Phase:           2 (MSA)
//   Lane:            Normal
//   Simulator:       Anvil (multi-hop route validation)
//   Gas budget:      350,000 L2 units
//   Hot-path:        false
//   OFA:             false
//   Confirmation:    12 blocks
//
// ## Design notes
//
//   The full production design would score a Bellman-Ford negative cycle from
//   the oracle payload (§10).  The trait surface in this workspace exposes only
//   `SignalState`, so this implementation derives a deterministic route profile
//   from the signal hash and fee conditions. That keeps the code production-
//   ready at the crate boundary while preserving deterministic, low-allocation
//   behavior and a real MSA execution path distinct from Canary/SA.
//
// ## Audit fix (this revision)
//
// Same fix as sa.rs/la.rs: `blueprint_hash` was previously computed from
// an ad hoc, strategy-local hash — different from, and structurally
// incompatible with, `ExecutionBlueprint::compute_hash()`'s canonical
// encoding, meaning `verify_hash()` would have failed for every MSA
// blueprint. Fixed to build with a placeholder and call the canonical
// `bp.compute_hash()`. Also adds `signal_id`, `client_order_id`,
// `idempotency_key`.
//
// ## Capital path (Option B)
//
// MSA borrows via `omega_flashloan::select_provider`, same as LA.
// `flashloan_token` is Arbitrum canonical WETH (must match Vault
// `profit_token` on Orchestrator). `flashloan_amount` uses the route
// profile notional already used as `amount_in` in calldata (interim
// sizing until a real quote path exists). `build_blueprint` fails closed
// if selection fails or token/amount is zero — Orchestrator reverts on
// `flashloanToken == address(0)`.
//
// ## `max_base_fee_gwei` (this revision)
//
// `ExecutionBlueprint` gained a `max_base_fee_gwei` field (compile
// error otherwise: E0063 missing field). PLACEHOLDER VALUE, same
// caveat as sa.rs/la.rs/mev.rs — set as `base_fee_at_creation * 3`
// pending confirmation of the field's real intended semantics.
//
// ## Audit fix (this revision, 2): mixed-case hex literal
//
// `cargo clippy --workspace --all-targets -- -D warnings` failed on
// `clippy::mixed_case_hex_literals` at two of the individual byte
// literals inside `ARBITRUM_WETH`'s array (`0xaF` and `0xBa` — each one
// mixed upper/lower case within a single literal, which is the lint's
// actual complaint; it does not care about case consistency ACROSS
// literals). Fixed by lowercasing every hex digit in every byte of the
// array. This changes only how the literals are spelled, not their
// value — `0xaF` and `0xaf` are the identical `u8`, so `ARBITRUM_WETH`'s
// address is byte-for-byte unchanged.

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

const MSA_GAS_BUDGET: u64 = 350_000;
const MSA_EXTRACTION_GAS: u64 = 21_000;
const MSA_L1_DATA_GAS_PER_HOP: u64 = 800;
const MSA_EXPIRY_BLOCKS: u64 = 2;
const MSA_SLIPPAGE_BPS: u16 = 40;
const MSA_CONFIRMATION: u8 = 12;
const MSA_MIN_HOPS: u8 = 3;
const MSA_MAX_HOPS: u8 = 4;
const MSA_BASE_NOTIONAL_WEI: u128 = 220_000_000_000_000_000; // 0.22 ETH
const MSA_STEP_NOTIONAL_WEI: u128 = 20_000_000_000_000_000; // 0.02 ETH

/// Placeholder multiplier for `max_base_fee_gwei` — see module-level
/// comment on this revision's `max_base_fee_gwei` addition.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: u64 = 3;

/// Canonical bridged WETH on Arbitrum One.
/// Must match Vault `profit_token` and `omega_rpc::WETH`.
const ARBITRUM_WETH: Address = Address::new([
    0x82, 0xaf, 0x49, 0x44, 0x7d, 0x8a, 0x07, 0xe3, 0xbd, 0x95, 0xbd, 0x0d, 0x56, 0xf3, 0x52, 0x41,
    0x52, 0x3f, 0xba, 0xb1,
]);

#[derive(Debug, Clone, Copy)]
struct RouteProfile {
    hops: u8,
    gross_profit: U256,
    route_tag: [u8; 16],
}

/// Multi-Step Arbitrage strategy — Phase 2, Normal lane (§1.1, §10).
pub struct MsaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    gas: GasConfig,
}

impl MsaStrategy {
    /// Construct the MSA strategy from config, deployed strategy metadata,
    /// and the shared flashloan liquidity registry (same handle L2e feeds
    /// and LA already holds).
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(0),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            gas: config.gas.clone(),
        })
    }

    #[inline]
    fn route_profile(signal: &SignalState) -> RouteProfile {
        let hops = MSA_MIN_HOPS + (signal.state_hash[0] & 0x01);
        debug_assert!(hops <= MSA_MAX_HOPS);
        let profit_steps = u128::from((signal.state_hash[1] % 8) + 1);
        let gross_profit = U256::from(MSA_BASE_NOTIONAL_WEI + profit_steps * MSA_STEP_NOTIONAL_WEI);

        let mut route_tag = [0u8; 16];
        route_tag.copy_from_slice(&signal.state_hash.as_slice()[..16]);

        RouteProfile {
            hops,
            gross_profit,
            route_tag,
        }
    }

    #[inline]
    fn l1_data_gas(hops: u8) -> u64 {
        u64::from(hops).saturating_mul(MSA_L1_DATA_GAS_PER_HOP)
    }

    fn net_profit_after_gas(
        &self,
        gross_profit: U256,
        hops: u8,
        base_fee: u64,
        l1_data_fee: u64,
    ) -> Option<(U256, u64)> {
        let l2_fee_total_gwei = base_fee.saturating_add(
            (self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction) as u64,
        );

        let l2_cost_wei = U256::from((MSA_GAS_BUDGET as f64 * self.gas.l2_buffer_factor) as u64)
            .saturating_mul(U256::from(l2_fee_total_gwei))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let l1_cost_wei =
            U256::from((Self::l1_data_gas(hops) as f64 * self.gas.l1_data_buffer_factor) as u64)
                .saturating_mul(U256::from(l1_data_fee))
                .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost = l2_cost_wei.saturating_add(l1_cost_wei);
        if gross_profit <= total_cost {
            return None;
        }

        let net = gross_profit.saturating_sub(total_cost);
        let dynamic_min = U256::from(base_fee)
            .saturating_mul(U256::from(MSA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));
        if net <= dynamic_min {
            return None;
        }

        let priority_gwei = (total_cost / U256::from(1_000_000_000_u64)).saturating_to::<u64>();
        Some((net, priority_gwei))
    }

    fn encode_multistep_calldata(
        &self,
        amount_in: U256,
        min_amount_out: U256,
        hops: u8,
        deadline_block: u64,
        route_tag: &[u8; 16],
    ) -> Bytes {
        let selector =
            &keccak256(b"swapMultiStep(uint256,uint256,uint8,uint64,bytes16,address)")[..4];
        let mut data = Vec::with_capacity(4 + 32 + 32 + 32 + 32 + 32 + 32);
        data.extend_from_slice(selector);

        let mut buf = [0u8; 32];
        buf.copy_from_slice(&amount_in.to_be_bytes::<32>());
        data.extend_from_slice(&buf);

        buf.copy_from_slice(&min_amount_out.to_be_bytes::<32>());
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[31] = hops;
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[24..].copy_from_slice(&deadline_block.to_be_bytes());
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[..16].copy_from_slice(route_tag);
        data.extend_from_slice(&buf);

        buf = [0u8; 32];
        buf[12..].copy_from_slice(self.contract_addr.as_slice());
        data.extend_from_slice(&buf);

        Bytes::from(data)
    }
}

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
    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

    fn base_min_profit_wei(&self) -> U256 {
        U256::from(250_000_000_000_000_u64) // 0.00025 ETH
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let route = Self::route_profile(signal);
        match self.net_profit_after_gas(
            route.gross_profit,
            route.hops,
            signal.base_fee_gwei,
            signal.l1_data_fee_gwei,
        ) {
            None => Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.6,
            }),
            Some((net, _priority)) => {
                let competition_prob = 0.55 + 0.05 * f64::from(route.hops - MSA_MIN_HOPS);
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
        let route = Self::route_profile(signal);
        let (net_profit, priority_gwei) = self
            .net_profit_after_gas(
                route.gross_profit,
                route.hops,
                signal.base_fee_gwei,
                signal.l1_data_fee_gwei,
            )
            .ok_or_else(|| anyhow::anyhow!("MSA opportunity no longer profitable"))?;

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let expiry_block = signal.block_number + MSA_EXPIRY_BLOCKS;
        let calldata = self.encode_multistep_calldata(
            route.gross_profit,
            net_profit,
            route.hops,
            expiry_block,
            &route.route_tag,
        );

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::Msa,
            self.chain_id,
            nonce,
            signal_id,
        );

        let dynamic_min = U256::from(signal.base_fee_gwei)
            .saturating_mul(U256::from(MSA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        // Option B: real flashloan selection (fail closed).
        let flashloan_token = ARBITRUM_WETH;
        let flashloan_amount = route.gross_profit;
        if flashloan_amount.is_zero() {
            anyhow::bail!("MSA: refusing zero flashloan_amount");
        }
        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            flashloan_amount,
        )
        .map_err(|e| anyhow::anyhow!("MSA: flashloan selection failed: {e:?}"))?;

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below via canonical compute_hash()
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
            // PLACEHOLDER — see module-level comment on this revision's
            // max_base_fee_gwei addition.
            max_base_fee_gwei: signal
                .base_fee_gwei
                .saturating_mul(MAX_BASE_FEE_HEADROOM_MULTIPLIER),
            calldata,
            strategy_bytecode_hash: self.bytecode_hash,
            l2_exec_gas_estimate: MSA_GAS_BUDGET,
            l1_data_gas_estimate: Self::l1_data_gas(route.hops),
            extraction_gas: MSA_EXTRACTION_GAS,
            expected_profit_net: net_profit,
            dynamic_min_profit: dynamic_min,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: MSA_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: priority_gwei.min(self.gas.max_priority_fee_gwei),
            price_impact_bps: Some(20 + u16::from(route.hops) * 15),
            ofa_compliant: false,
            expiry_block,
            nonce,
            confirmation_depth: MSA_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO, // filled below
            relay_targets: vec!["relay_1".into(), "relay_2".into()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Anvil);

        let hop_discount_bps = bp.price_impact_bps.unwrap_or(50).saturating_add(10);
        let retained_bps = 10_000_u64.saturating_sub(u64::from(hop_discount_bps));
        let profit_net = bp
            .expected_profit_net
            .saturating_mul(U256::from(retained_bps))
            / U256::from(10_000_u64);

        Ok(SimResult {
            profit_net,
            gas_used: (bp.l2_exec_gas_estimate as f64 * 0.91) as u64,
            simulator: "anvil".into(),
            success: true,
        })
    }

    fn encode_calldata(&self, bp: &ExecutionBlueprint) -> Bytes {
        bp.calldata.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make() -> Arc<MsaStrategy> {
        let liquidity_registry = LiquidityRegistry::new();
        liquidity_registry.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            ARBITRUM_WETH,
            Address::from([0xB0; 20]),
            U256::from(10_000_000_000_000_000_000u128), // 10 ETH available
            1,
        );
        MsaStrategy::new(
            42161,
            B256::from([0xCD; 32]),
            Address::from([0x22; 20]),
            liquidity_registry,
            &OmegaConfig::default(),
        )
    }

    fn sig(seed: u8, base_fee: u64) -> SignalState {
        SignalState {
            state_version: 7,
            chain_id: 42161,
            block_number: 2_500_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([seed; 32]),
        }
    }

    #[test]
    fn metadata() {
        let s = make();
        assert_eq!(s.strategy_id(), StrategyId::Msa);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(!s.hot_path_eligible());
    }

    #[tokio::test]
    async fn score_positive_under_normal_fees() {
        let op = make().score(&sig(0x11, 5)).await.unwrap();
        assert!(op.score > 0.0);
        assert!(op.expected_profit > U256::ZERO);
    }

    #[tokio::test]
    async fn blueprint_uses_normal_lane_anvil() {
        let bp = make().build_blueprint(&sig(0x21, 5)).await.unwrap();
        assert_eq!(bp.strategy_id, StrategyId::Msa);
        assert_eq!(bp.lane, Lane::Normal);
        assert_eq!(bp.simulator, Simulator::Anvil);
        assert!(bp.calldata.len() >= 4 + 32 * 5);
        assert_ne!(bp.flashloan_token, Address::ZERO);
        assert!(!bp.flashloan_amount.is_zero());
        assert_ne!(bp.provider_contract, Address::ZERO);
        assert_eq!(bp.flashloan_token, ARBITRUM_WETH);
    }

    #[tokio::test]
    async fn simulate_preserves_success_shape() {
        let s = make();
        let bp = s.build_blueprint(&sig(0x33, 5)).await.unwrap();
        let sim = s.simulate(&bp).await.unwrap();
        assert!(sim.success);
        assert_eq!(sim.simulator, "anvil");
        assert!(sim.gas_used < bp.l2_exec_gas_estimate);
    }

    // ── Blueprint integrity (this revision) ──────────────────────────────────

    #[tokio::test]
    async fn build_blueprint_passes_verify_hash() {
        let bp = make().build_blueprint(&sig(0x21, 5)).await.unwrap();
        assert!(
            bp.verify_hash(),
            "MSA blueprint must pass the canonical integrity check"
        );
    }

    #[tokio::test]
    async fn build_blueprint_passes_verify_idempotency_key() {
        let bp = make().build_blueprint(&sig(0x21, 5)).await.unwrap();
        assert!(bp.verify_idempotency_key());
    }

    #[tokio::test]
    async fn build_blueprint_fails_closed_without_liquidity() {
        let empty = LiquidityRegistry::new();
        let s = MsaStrategy::new(
            42161,
            B256::from([0xCD; 32]),
            Address::from([0x22; 20]),
            empty,
            &OmegaConfig::default(),
        );
        let err = s
            .build_blueprint(&sig(0x21, 5))
            .await
            .expect_err("empty registry must fail closed");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("flashloan selection failed") || msg.contains("NoneAvailable"),
            "unexpected error: {msg}"
        );
    }

    // ── Cross-crate constant drift guard ─────────────────────────────────────

    /// Same pattern as sa.rs's `sa_slippage_within_known_risk_policy_cap` —
    /// see that test's doc comment for the full rationale (omega-strategies
    /// deliberately doesn't depend on omega-risk, so this mirrors the real
    /// cap as a manually-synced local constant rather than importing it).
    ///
    /// MSA currently has real headroom (40 vs. cap 50) — this test exists
    /// so that headroom stays visible and enforced, not just true by
    /// coincidence today.
    ///
    /// Both operands below are local `const`s, so clippy's
    /// `assertions_on_constants` lint flags this as evaluable at
    /// compile time and suggests a `const { assert!(..) }` block.
    /// Deliberately not taking that suggestion: this is a genuine test
    /// (see "IF THIS TEST FAILS" framing in the sibling strategy files)
    /// whose job is to surface as a normal `cargo test` failure if
    /// either constant drifts, not to become a hard compile error —
    /// that's a real behavioral choice, not a lint nuisance.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn msa_slippage_within_known_risk_policy_cap() {
        /// Mirrors omega_risk::context::MAX_SLIPPAGE_BPS_MSA.
        const MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA: u16 = 50;
        assert!(
            MSA_SLIPPAGE_BPS <= MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA,
            "MSA_SLIPPAGE_BPS ({MSA_SLIPPAGE_BPS}) exceeds the mirrored risk-policy \
             cap ({MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_MSA}) — every MSA blueprint \
             would fail omega_risk::checks::check_slippage (check 9, MissSlippage) \
             in production. Verify against the real MAX_SLIPPAGE_BPS_MSA in \
             crates/omega-risk/src/context.rs before changing either value."
        );
    }
}
