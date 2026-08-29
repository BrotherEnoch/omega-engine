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
// ## Flashloan selection wiring (earlier revision)
//
// `build_blueprint` calls `omega_flashloan::select_provider` for real,
// replacing the fixed constructor-injected `flashloan_provider` address
// with genuine Balancer -> AaveV3 -> UniswapV3 fallback selection.
// `flashloan_provider_type` / `provider_contract` on `ExecutionBlueprint`
// are populated from the real `SelectionResult`.
//
// ## Real position selection + real flashloan_token (this revision)
//
// LA now holds `Arc<omega_positions::PositionRegistry>` and, on every
// `score`/`build_blueprint` call, selects the most urgent currently-
// liquidatable position (`PositionRegistry::liquidatable_positions`,
// already sorted ascending by health factor) instead of operating
// against the fixed `LA_PROXY_DEBT_WEI` constant every cycle regardless
// of any real position existing. This closes the `flashloan_token` gap
// this file has carried since real flashloan-provider selection was
// wired in: `PositionSnapshot` (omega-core) now carries real
// `debt_token`/`collateral_token` fields, sourced from a real selected
// position, so `debt_token()` below is no longer an unconditional
// `None` — see that function's own doc comment.
//
// KNOWN LIMITATION, new this revision, not previously possible to hit:
// `score` and `build_blueprint` are two SEPARATE calls (per
// `StrategyTrait`'s own documented execution flow) with no shared
// state between them — each independently queries
// `PositionRegistry::liquidatable_positions` and takes the current
// front of that list. If the registry's contents change between the
// two calls (a new poll cycle updates or evicts the position that was
// scored), `build_blueprint` can end up building against a DIFFERENT
// position than the one `score` evaluated. This is not a new class of
// risk for this codebase (`SignalState` itself can already change
// between the two calls for every strategy), but it's flagged
// explicitly here since it's new to LA specifically with this
// revision. Fixing it would need `StrategyTrait`'s signature to thread
// the scored opportunity through to `build_blueprint` — the same
// cross-strategy trait-change cost already avoided when
// `PositionRegistry` was designed as a side-channel rather than a new
// trait parameter (see that crate's own doc comment).
//
// ## STILL NOT RESOLVED: `flashloan_token`'s AMOUNT, `debt_amount_wei`
//
// A real, selected `PositionSnapshot` gives LA a real `debt_token` and
// a real `debt_usd_e18` (a USD VALUE). It does NOT give LA a wei amount
// of that token to actually borrow — that conversion needs a live
// price for `debt_token` (USD-per-token, and that token's decimals),
// which nothing in `LaStrategy` has access to; no price-oracle client
// is wired into this strategy at all. `debt_amount_wei` below is
// always `None` today for exactly this reason, and both `score` and
// `build_blueprint` now genuinely refuse whenever it's `None` — see
// that function's own doc comment for the full reasoning, including
// why guessing here would be strictly worse than refusing (a real
// selected position paired with a fabricated amount).
//
// PRACTICAL CONSEQUENCE OF THIS CHOICE: `score` now returns `0.0` for
// EVERY liquidatable position tracked, until a real price source is
// wired in — this is a deliberate, honest regression from the prior
// revision's behavior (which always scored something nonzero off the
// fake `LA_PROXY_DEBT_WEI` constant, real position or not). `LA_PROXY_
// DEBT_WEI` itself is REMOVED this revision — it no longer has any
// caller; real position debt sizing is the only path now, gated
// correctly rather than silently bypassed.
//
// ## `max_base_fee_gwei` (earlier revision)
//
// `ExecutionBlueprint` gained a `max_base_fee_gwei` field. PLACEHOLDER
// VALUE — the real semantics of this field haven't been confirmed
// against whatever consumes it downstream. Set here as
// `base_fee_at_creation * 3`, mirroring the kind of headroom
// `l2_buffer_factor`/`l1_data_buffer_factor` already apply elsewhere in
// this file — a guess, not a derived value.
//
// ## STILL UNRESOLVED, UNCHANGED BY THIS REVISION: calldata ABI mismatch
//
// `encode_liquidation_calldata` below encodes a selector for
// `liquidate(uint256,uint256)` — this does NOT match the real deployed
// `LiquidationArb.sol::execute(bytes,uint256)` ABI (which decodes
// `(Protocol, collateral, debt, user, debtToCover, minProfit,
// extraData)`). Flagged in this codebase's own investigation history;
// not fixed here — this revision's scope is position/token sourcing,
// not the calldata encoder. A resolved `debt_amount_wei` gap does NOT
// make this blueprint's calldata correct against the real contract.

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
use omega_core::types::oracle::PositionSnapshot;
use omega_core::types::strategy::{OpScore, SignalState, SimResult, StrategyTrait};
use omega_core::{GasConfig, OmegaConfig};
use omega_flashloan::LiquidityRegistry;
use omega_positions::PositionRegistry;

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
    /// Real, live tracked lending-position registry (this revision) —
    /// see `omega_positions::PositionRegistry`'s own doc comment. Read
    /// on every `score`/`build_blueprint` call via
    /// `liquidatable_positions(self.chain_id)`; nothing here writes to
    /// it (writer is an omega-oracle component not part of this crate).
    position_registry: Arc<PositionRegistry>,
    gas: GasConfig,
}

impl LaStrategy {
    /// CONSTRUCTOR SIGNATURE CHANGED (this revision): gains
    /// `position_registry: Arc<PositionRegistry>`. Every call site
    /// constructing `LaStrategy` must be updated — per earlier
    /// verification in this codebase's own history, `main.rs` does not
    /// currently construct LA at all (only CNRY is registered in
    /// production), so this affects test helpers only, not live wiring,
    /// as of this revision.
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        liquidity_registry: Arc<LiquidityRegistry>,
        position_registry: Arc<PositionRegistry>,
        config: &OmegaConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(0),
            bytecode_hash,
            contract_addr,
            liquidity_registry,
            position_registry,
            gas: config.gas.clone(),
        })
    }

    /// Returns true when the health factor is in the hot tier (HF < 1.01).
    pub fn is_hot_tier(hf_e18: U256) -> bool {
        hf_e18.saturating_to::<u128>() < HOT_TIER_HF_THRESHOLD
    }

    /// Selects the position LA would act on THIS cycle: the most urgent
    /// (lowest health factor) currently-liquidatable position tracked
    /// for this strategy's chain. `None` when no liquidatable position
    /// is currently tracked — a legitimate, expected state (no
    /// opportunity right now), not an error.
    ///
    /// See this file's own module-level "KNOWN LIMITATION" comment:
    /// `score` and `build_blueprint` each call this independently, so
    /// they are not guaranteed to select the SAME position if the
    /// registry changes between the two calls.
    fn select_position(&self) -> Option<PositionSnapshot> {
        self.position_registry
            .liquidatable_positions(self.chain_id)
            .into_iter()
            .next()
    }

    /// Real source for `flashloan_token`, once a position is selected —
    /// no longer an unconditional `None`. Returns `Some` only when the
    /// selected position's `debt_token` is non-zero: a zero address on
    /// a real `PositionSnapshot` would indicate the upstream oracle
    /// writer itself has a gap, and this function refuses to pass that
    /// through as if it were a valid token, same "never launder an
    /// invalid upstream value into a blueprint" posture as every other
    /// guard in this file.
    fn debt_token(position: &PositionSnapshot) -> Option<Address> {
        if position.debt_token == Address::ZERO {
            None
        } else {
            Some(position.debt_token)
        }
    }

    /// Converts `position.debt_usd_e18` (a USD VALUE) into the exact
    /// wei amount of `position.debt_token` needed to cover it. Always
    /// `None` today: this requires a live price for `debt_token`
    /// (USD-per-token, plus that token's decimals) that nothing in
    /// `LaStrategy` has access to — no price-oracle client is wired
    /// into this strategy at all.
    ///
    /// Refusing to fabricate a number here is deliberate, not an
    /// oversight: `debt_usd_e18` is a USD value, not a token amount,
    /// and guessing a price (or assuming 18 decimals, or assuming
    /// $1 == 1 token) would silently mis-size every flash-borrow —
    /// potentially borrowing far more or less than the position's real
    /// debt, now with a REAL borrower address and REAL debt token
    /// attached to the wrong number. That combination (real identity,
    /// fabricated amount) is strictly worse than either being honestly
    /// absent — same reasoning already applied elsewhere in this
    /// codebase's history to a fake-but-present signal.
    ///
    /// ALSO NOT ADDRESSED by fixing this alone: even a correct
    /// token-wei conversion here would not, by itself, make
    /// `net_profit_after_gas`'s arithmetic correct — that function
    /// compares the flash-borrowed amount directly against
    /// ETH-denominated gas costs, implicitly assuming the debt token
    /// IS ETH-equivalent wei. That unit conflation predates this
    /// revision (it already existed when the amount was the flat
    /// `LA_PROXY_DEBT_WEI` constant) and is not fixed here — flagged,
    /// not resolved, since fixing it needs the same missing price
    /// source this function is itself blocked on.
    fn debt_amount_wei(&self, _position: &PositionSnapshot) -> Option<U256> {
        None
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
        let Some(position) = self.select_position() else {
            // No liquidatable position currently tracked — a legitimate
            // "no opportunity this cycle" state, not an error.
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            });
        };

        let Some(debt_wei) = self.debt_amount_wei(&position) else {
            // Position is real and liquidatable, but no price source
            // exists to size the flash-borrow — see debt_amount_wei's
            // own doc comment. Same "no opportunity right now" shape,
            // not an error: the position may become actionable once a
            // price source exists, without this being a hard failure.
            tracing::debug!(
                borrower = %position.borrower,
                protocol = %position.protocol,
                "LA: liquidatable position found but no debt-amount pricing source \
                 available — scoring as no-opportunity"
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            });
        };

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
        let position = self.select_position().ok_or_else(|| {
            anyhow::anyhow!(
                "LA: no liquidatable position currently tracked for chain {}; \
                 refusing to build a blueprint with no real position behind it",
                self.chain_id
            )
        })?;

        // GUARD: debt_token is now real (sourced from the selected
        // position) — see debt_token's own doc comment for why this
        // can still legitimately fail (a zero address on the upstream
        // snapshot).
        let flashloan_token = Self::debt_token(&position).ok_or_else(|| {
            anyhow::anyhow!(
                "LA: selected position (borrower {:?}, protocol {:?}) has no valid \
                 debt_token — refusing to build a blueprint with a zero-address token",
                position.borrower,
                position.protocol
            )
        })?;

        // GUARD: no price source exists yet to size the flash-borrow —
        // see debt_amount_wei's own doc comment for the full reasoning
        // on why this refuses rather than guesses.
        let debt_wei = self.debt_amount_wei(&position).ok_or_else(|| {
            anyhow::anyhow!(
                "LA: no debt-amount pricing source available for position (borrower {:?}, \
                 protocol {:?}, debt_token {:?}). debt_amount_wei requires a live price for \
                 that token that nothing in LaStrategy has access to today — refusing to \
                 build a blueprint with a fabricated or guessed flash-borrow amount; \
                 flashloan_token is real (see `flashloan_token` above) but cannot be sized \
                 without this.",
                position.borrower,
                position.protocol,
                flashloan_token,
            )
        })?;

        let (net_profit, priority_gwei) = self
            .net_profit_after_gas(
                debt_wei,
                LA_LIQUIDATION_BONUS_FRAC,
                signal.base_fee_gwei,
                signal.l1_data_fee_gwei,
            )
            .ok_or_else(|| anyhow::anyhow!("LA opportunity no longer profitable"))?;

        // Real provider/pool selection — see module-level comment.
        let selection =
            omega_flashloan::select_provider(&self.liquidity_registry, self.chain_id, debt_wei)
                .map_err(|e| anyhow::anyhow!("LA: flashloan selection failed: {e:?}"))?;

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
            // existing reader.
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
    use omega_core::types::oracle::{PositionFinancials, PositionTokens};
    use omega_core::OmegaConfig;

    const TEST_CHAIN_ID: u64 = 42161;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    /// Builds a real `PositionSnapshot` via its own canonical constructor.
    fn sample_position(hf_e18: u128) -> PositionSnapshot {
        PositionSnapshot::new(
            addr(0x01),
            addr(0x02),
            U256::from(hf_e18),
            PositionFinancials {
                collateral_usd_e18: U256::from(2_000_000_000_000_000_000u128),
                debt_usd_e18: U256::from(1_000_000_000_000_000_000u128),
                liquidation_bonus_bps: 500,
            },
            PositionTokens {
                debt_token: addr(0xD0),
                collateral_token: addr(0xC0),
            },
            3_000_000,
            1,
        )
    }

    fn make_with_positions(positions: &[PositionSnapshot]) -> Arc<LaStrategy> {
        let liquidity_registry = LiquidityRegistry::new();
        // Seeded even though these tests never reach flashloan selection
        // (they're all gated earlier, on position/pricing) — kept for
        // parity with the constructor's real requirements and in case a
        // future test extends past those guards.
        liquidity_registry.update(
            TEST_CHAIN_ID,
            omega_flashloan::FlashloanProvider::Balancer,
            Address::from([0xB0; 20]),
            U256::from(1_000_000_000_000_000_000_000u128),
            1,
        );

        let position_registry = PositionRegistry::new();
        for p in positions {
            position_registry.update(TEST_CHAIN_ID, p.clone());
        }

        LaStrategy::new(
            TEST_CHAIN_ID,
            B256::from([0xAB; 32]),
            Address::ZERO,
            liquidity_registry,
            position_registry,
            &OmegaConfig::default(),
        )
    }

    fn make() -> Arc<LaStrategy> {
        make_with_positions(&[])
    }

    fn sig(base_fee: u64) -> SignalState {
        SignalState {
            state_version: 1,
            chain_id: TEST_CHAIN_ID,
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

    /// Regression guard for this revision: with no liquidatable position
    /// tracked, score must report no opportunity — not fabricate one off
    /// a fixed constant the way the prior revision did.
    #[tokio::test]
    async fn score_zero_when_no_liquidatable_position() {
        let op = make().score(&sig(5)).await.unwrap();
        assert_eq!(op.score, 0.0);
        assert_eq!(op.expected_profit, U256::ZERO);
    }

    /// Regression guard for this revision: even with a real, liquidatable
    /// position tracked, score must still report no opportunity while no
    /// debt-amount pricing source exists — this is the honest, current
    /// state (see `debt_amount_wei`'s own doc comment), not a bug.
    #[tokio::test]
    async fn score_zero_when_position_present_but_unpriced() {
        let strat = make_with_positions(&[sample_position(
            1_000_000_000_000_000_000u128 - 1, // liquidatable
        )]);
        let op = strat.score(&sig(5)).await.unwrap();
        assert_eq!(
            op.score, 0.0,
            "no price source exists yet — a real position must not produce a fabricated score"
        );
    }

    /// Regression guard for this revision: build_blueprint must fail
    /// cleanly, not panic, when no liquidatable position is tracked.
    #[tokio::test]
    async fn build_blueprint_fails_without_liquidatable_position() {
        let bp_result = make().build_blueprint(&sig(5)).await;
        assert!(
            bp_result.is_err(),
            "LA must refuse to build with no liquidatable position tracked"
        );
        let msg = bp_result.unwrap_err().to_string();
        assert!(msg.contains("liquidatable position"), "error message: {msg}");
    }

    /// Regression guard for this revision: build_blueprint must fail
    /// cleanly (not panic, not fabricate an amount) while
    /// debt_amount_wei has no real source — this is the expected,
    /// current, honest state, now gated on PRICING rather than on the
    /// token itself (the token is real as of this revision).
    #[tokio::test]
    async fn build_blueprint_fails_without_debt_amount_pricing_source() {
        let strat = make_with_positions(&[sample_position(1_000_000_000_000_000_000u128 - 1)]);
        let bp_result = strat.build_blueprint(&sig(5)).await;
        assert!(
            bp_result.is_err(),
            "LA must refuse to build until a real debt-amount pricing source exists"
        );
        let msg = bp_result.unwrap_err().to_string();
        assert!(msg.contains("pricing"), "error message: {msg}");
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

    // ── Cross-crate constant drift guard ─────────────────────────────────────

    /// Same pattern as sa.rs's `sa_slippage_within_known_risk_policy_cap` —
    /// see that test's doc comment for the full rationale (omega-strategies
    /// deliberately doesn't depend on omega-risk, so this mirrors the real
    /// cap as a manually-synced local constant rather than importing it).
    ///
    /// LA currently has the most headroom of any strategy (50 vs. cap
    /// 100) — this test exists so that headroom stays visible and
    /// enforced, not just true by coincidence today.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn la_slippage_within_known_risk_policy_cap() {
        /// Mirrors omega_risk::context::MAX_SLIPPAGE_BPS_LA.
        const MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_LA: u16 = 100;
        assert!(
            LA_SLIPPAGE_BPS <= MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_LA,
            "LA_SLIPPAGE_BPS ({LA_SLIPPAGE_BPS}) exceeds the mirrored risk-policy \
             cap ({MIRRORED_CONTEXT_RS_MAX_SLIPPAGE_BPS_LA}) — every LA blueprint \
             would fail omega_risk::checks::check_slippage (check 9, MissSlippage) \
             in production. Verify against the real MAX_SLIPPAGE_BPS_LA in \
             crates/omega-risk/src/context.rs before changing either value."
        );
    }
}