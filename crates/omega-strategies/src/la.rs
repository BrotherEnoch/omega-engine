// crates/omega-strategies/src/la.rs
//
// Liquidation Arbitrage (LA) — Phase 3 strategy (spec §1.1, §11).
//
// ## Audit fix (earlier revision)
//
// Same fix as sa.rs: `blueprint_hash` was previously computed from an ad
// hoc, strategy-local `keccak256(signal_state_hash || nonce ||
// debt_wei)` — different from, and much smaller than,
// `ExecutionBlueprint::compute_hash()`'s canonical encoding. Fixed to
// build with a placeholder and call the canonical `bp.compute_hash()`.
// Also adds `signal_id`, `client_order_id`, `idempotency_key`.
//
// ## Flashloan selection wiring (this revision)
//
// `build_blueprint` now calls `omega_flashloan::select_provider` for
// real, replacing the fixed constructor-injected `flashloan_provider`
// address with genuine Balancer -> AaveV3 -> UniswapV3 fallback
// selection. This required:
//   - `LaStrategy::new` now takes `Arc<LiquidityRegistry>` instead of a
//     fixed `flashloan_provider: Address` — a real constructor
//     signature change; update all call sites.
//   - New dependency edge: `omega-strategies` -> `omega-flashloan` in
//     Cargo.toml.
//   - `flashloan_provider_type` / `provider_contract` on
//     `ExecutionBlueprint` are populated from the real
//     `SelectionResult`, mapped via `flashloan_select::to_blueprint_provider_type`.
//
// ## Known incomplete: `flashloan_token` (this revision)
//
// `select_provider`'s real signature — `select_provider(registry,
// chain_id, amount_wei)` — takes no token argument, and
// `LiquidityRegistry` tracks no token either (verified against
// `crates/omega-flashloan/src/tests.rs`). Selection picks a
// provider/pool, not an asset. LA has no source for which ERC20 token
// the liquidated position's debt is denominated in — `PositionSnapshot`
// (omega-core) carries `debt_usd_e18` (a USD value) and no token
// address field. Rather than guess, `build_blueprint` returns an
// explicit error when a real token source isn't available. This blocks
// on the same gap tracked under "Problem 2" (position-data injection) —
// fixing that is very likely a prerequisite for `flashloan_token` too,
// not a separate independent task.
//
// ## Still unresolved from the prior revision (unchanged)
//
// `LA_PROXY_DEBT_WEI` is still a fixed constant standing in for real
// position debt — see that constant's own comment. This revision does
// NOT fix that; it only wires provider selection around the same
// (still-fake) debt amount.
//
// ## `max_base_fee_gwei` (this revision)
//
// `ExecutionBlueprint` gained a `max_base_fee_gwei` field (compile
// error otherwise: E0063 missing field). PLACEHOLDER VALUE — the real
// semantics of this field (submission-time fee ceiling? relay
// rejection threshold above which the bundle is dropped?) haven't been
// confirmed against whatever consumes it downstream. Set here as
// `base_fee_at_creation * 3`, mirroring the kind of headroom
// `l2_buffer_factor`/`l1_data_buffer_factor` already apply elsewhere in
// this file, but this is a guess, not a derived value — confirm the
// intended cap logic before relying on it in production.

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

const LA_GAS_BUDGET: u64 = 600_000;
const LA_EXTRACTION_GAS: u64 = 21_000;
const LA_L1_DATA_GAS: u64 = 3_200;
const LA_EXPIRY_BLOCKS: u64 = 1;
const LA_SLIPPAGE_BPS: u16 = 50;
const LA_CONFIRMATION: u8 = 12;
const LA_LIQUIDATION_BONUS_FRAC: f64 = 0.10;
// TODO(position-sizing, not fixed in this revision): this is a fixed
// proxy value, not derived from any real liquidation position. Every
// LA blueprint currently flash-borrows exactly this amount regardless
// of the actual position's debt size. A real implementation needs this
// sourced from PositionSnapshot.debt_usd_e18 (crates/omega-core/src/
// types/oracle.rs) or equivalent — see the module-level comment above
// on why this is entangled with the flashloan_token gap too.
const LA_PROXY_DEBT_WEI: u128 = 5_000_000_000_000_000_000;

/// Health-factor threshold for "hot tier" (< 1.01 × 1e18).
const HOT_TIER_HF_THRESHOLD: u128 = 1_010_000_000_000_000_000;

/// Placeholder multiplier for `max_base_fee_gwei` — see module-level
/// comment on this revision's `max_base_fee_gwei` addition.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: u64 = 3;

// ─────────────────────────────────────────────────────────────────────────────
// LaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct LaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    contract_addr: Address,
    liquidity_registry: Arc<LiquidityRegistry>,
    gas: GasConfig,
}

impl LaStrategy {
    /// CONSTRUCTOR SIGNATURE CHANGED (this revision): `flashloan_provider:
    /// Address` replaced with `liquidity_registry: Arc<LiquidityRegistry>`.
    /// Every call site constructing `LaStrategy` must be updated — per
    /// earlier verification in this thread, `main.rs` does not currently
    /// construct LA at all (only CNRY is registered in production), so
    /// this affects test helpers only, not live wiring, as of this
    /// revision.
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

    /// Returns true when the health factor is in the hot tier (HF < 1.01).
    pub fn is_hot_tier(hf_e18: U256) -> bool {
        hf_e18.saturating_to::<u128>() < HOT_TIER_HF_THRESHOLD
    }

    fn net_profit_after_gas(
        &self,
        debt_wei: U256,
        bonus_frac: f64,
        base_fee: u64,
        l1_data_fee: u64,
    ) -> Option<(U256, u64)> {
        let gross = U256::from((debt_wei.saturating_to::<u128>() as f64 * bonus_frac) as u128);

        let l2_fee_gwei = base_fee.saturating_add(
            (self.gas.max_priority_fee_gwei as f64 * self.gas.conservative_fee_fraction) as u64,
        );
        let l2_cost = U256::from((LA_GAS_BUDGET as f64 * self.gas.l2_buffer_factor) as u64)
            .saturating_mul(U256::from(l2_fee_gwei))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let l1_cost = U256::from((LA_L1_DATA_GAS as f64 * self.gas.l1_data_buffer_factor) as u64)
            .saturating_mul(U256::from(l1_data_fee))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let total_cost = l2_cost.saturating_add(l1_cost);
        if gross <= total_cost {
            return None;
        }

        let net = gross.saturating_sub(total_cost);
        let dynamic = U256::from(base_fee)
            .saturating_mul(U256::from(LA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));
        if net <= dynamic {
            return None;
        }

        let priority_gwei: u64 = (total_cost / U256::from(1_000_000_000_u64)).saturating_to();
        Some((net, priority_gwei))
    }

    fn encode_liquidation_calldata(
        contract_addr: Address,
        debt_amount: U256,
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

    /// Placeholder for the real debt-token source. Always `None` today —
    /// see the module-level "Known incomplete: flashloan_token" comment.
    /// Exists as a named function (rather than an inline `None` in
    /// `build_blueprint`) so it's a single, greppable place to implement
    /// the real answer once one exists, and so the guard in
    /// `build_blueprint` reads as intentional rather than as a stray
    /// `unwrap`-shaped landmine.
    fn debt_token(&self, _signal: &SignalState) -> Option<Address> {
        None
    }
}

#[async_trait]
impl StrategyTrait for LaStrategy {
    fn strategy_id(&self) -> StrategyId {
        StrategyId::La
    }
    fn lane(&self) -> Lane {
        Lane::Normal
    }
    fn hot_path_eligible(&self) -> bool {
        true
    }
    fn gas_budget(&self) -> u64 {
        LA_GAS_BUDGET
    }
    fn expected_bytecode_hash(&self) -> B256 {
        self.bytecode_hash
    }

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
        let debt_wei = U256::from(LA_PROXY_DEBT_WEI);
        let (net_profit, priority_gwei) = self
            .net_profit_after_gas(
                debt_wei,
                LA_LIQUIDATION_BONUS_FRAC,
                signal.base_fee_gwei,
                signal.l1_data_fee_gwei,
            )
            .ok_or_else(|| anyhow::anyhow!("LA opportunity no longer profitable"))?;

        // Real provider/pool selection (this revision) — replaces the old
        // fixed constructor-injected address.
        let selection =
            omega_flashloan::select_provider(&self.liquidity_registry, self.chain_id, debt_wei)
                .map_err(|e| anyhow::anyhow!("LA: flashloan selection failed: {e:?}"))?;

        // GUARD: no source for the debt token exists yet. Refuse to build
        // rather than assign a wrong or placeholder address — same
        // reasoning as the debt_usd_e18-unit guard discussed earlier in
        // this thread (never silently emit a field with no real source
        // behind it), applied here for real.
        let flashloan_token = self.debt_token(signal).ok_or_else(|| {
            anyhow::anyhow!(
                "LA: no debt-token source available for this position. \
                 flashloan_token requires knowing which ERC20 the liquidated \
                 position's debt is denominated in — PositionSnapshot carries \
                 debt_usd_e18 (a USD value) only, no token address field. \
                 Refusing to build a blueprint with a fabricated or zero token \
                 address; provider/pool selection succeeded (see `selection`) \
                 but cannot be encoded without this."
            )
        })?;

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let calldata = Self::encode_liquidation_calldata(self.contract_addr, debt_wei, net_profit);

        let signal_id = Uuid::new_v4();
        let client_order_id = ExecutionBlueprint::derive_client_order_id(
            StrategyId::La,
            self.chain_id,
            nonce,
            signal_id,
        );

        let dynamic_min = U256::from(signal.base_fee_gwei)
            .saturating_mul(U256::from(LA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));

        let mut bp = ExecutionBlueprint {
            blueprint_hash: B256::ZERO, // filled below via canonical compute_hash()
            chain_id: self.chain_id,
            strategy_id: StrategyId::La,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            // Legacy field — kept populated with the same value as
            // provider_contract for backward compatibility with any
            // existing reader (see blueprint_field_patch_final.md: "do
            // not remove flashloan_provider yet, migrate readers first").
            flashloan_provider: selection.contract_addr,
            flashloan_amount: debt_wei,
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
            l2_exec_gas_estimate: LA_GAS_BUDGET,
            l1_data_gas_estimate: LA_L1_DATA_GAS,
            extraction_gas: LA_EXTRACTION_GAS,
            expected_profit_net: net_profit,
            dynamic_min_profit: dynamic_min,
            l2_buffer_factor: self.gas.l2_buffer_factor,
            l1_data_buffer_factor: self.gas.l1_data_buffer_factor,
            slippage_bps: LA_SLIPPAGE_BPS,
            base_fee_at_creation: signal.base_fee_gwei,
            l1_data_fee_at_creation: signal.l1_data_fee_gwei,
            priority_fee_gwei: priority_gwei.min(self.gas.max_priority_fee_gwei),
            price_impact_bps: None,
            ofa_compliant: false,
            expiry_block: signal.block_number + LA_EXPIRY_BLOCKS,
            nonce,
            confirmation_depth: LA_CONFIRMATION,
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
        let gas_used = (bp.l2_exec_gas_estimate as f64 * 0.88) as u64;
        Ok(SimResult {
            profit_net: bp.expected_profit_net,
            gas_used,
            simulator: "anvil".into(),
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

    fn make() -> Arc<LaStrategy> {
        let registry = LiquidityRegistry::new();
        // Seed enough Balancer liquidity to cover LA_PROXY_DEBT_WEI so
        // provider selection itself succeeds in tests. Real registry
        // population is a separate, not-yet-identified production
        // concern (same category as the PIL/no-flashloan-path gap).
        registry.update(
            42161,
            omega_flashloan::FlashloanProvider::Balancer,
            Address::from([0xB0; 20]),
            U256::from(LA_PROXY_DEBT_WEI * 10),
            1,
        );
        LaStrategy::new(
            42161,
            B256::from([0xAB; 32]),
            Address::ZERO,
            registry,
            &OmegaConfig::default(),
        )
    }

    fn sig(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 1,
            chain_id: 42161,
            block_number: 3_000_000,
            base_fee_gwei: base_fee,
            l1_data_fee_gwei: 2,
            state_hash: B256::from([0x03; 32]),
        }
    }

    #[test]
    fn metadata() {
        let s = make();
        assert_eq!(s.strategy_id(), StrategyId::La);
        assert_eq!(s.lane(), Lane::Normal);
        assert!(s.hot_path_eligible());
    }

    #[tokio::test]
    async fn score_positive_low_fee() {
        let op = make().score(&sig(5)).await.unwrap();
        assert!(op.score > 0.0);
    }

    /// Regression guard for this revision: build_blueprint must fail
    /// cleanly (not panic, not fabricate a token) while flashloan_token
    /// has no real source — this is the expected, current, honest state.
    #[tokio::test]
    async fn build_blueprint_fails_without_debt_token_source() {
        let bp_result = make().build_blueprint(&sig(5)).await;
        assert!(
            bp_result.is_err(),
            "LA must refuse to build until a real debt-token source exists"
        );
        let msg = bp_result.unwrap_err().to_string();
        assert!(msg.contains("debt-token"), "error message: {msg}");
    }

    #[test]
    fn hot_tier_detection() {
        let e18 = 1_000_000_000_000_000_000_u128;
        let hf_hot = U256::from(e18 + e18 / 1000);
        let hf_warm = U256::from(e18 + 5 * e18 / 100);
        assert!(LaStrategy::is_hot_tier(hf_hot), "1.001 should be hot tier");
        assert!(
            !LaStrategy::is_hot_tier(hf_warm),
            "1.05 should not be hot tier"
        );
    }
}
