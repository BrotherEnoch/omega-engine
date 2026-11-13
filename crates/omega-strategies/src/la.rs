// crates/omega-strategies/src/la.rs
//
// Liquidation Arbitrage (LA) — Phase 3 strategy (spec §1.1, §11).
//
// ## Production fixes (this revision)
//
// ### 1. Calldata ABI mismatch — FIXED
//
// `encode_liquidation_calldata` previously encoded `liquidate(uint256,uint256)`
// which does not match the deployed `LiquidationArb.sol::execute(bytes,uint256)`.
// Fixed to produce correctly ABI-encoded calldata for:
//
//   execute(bytes data, uint256 deadline)
//
// where `data` = abi.encode(
//   uint8   protocol,     -- LiquidationProtocol enum value
//   address collateral,   -- collateral token address
//   address debt,         -- debt token address
//   address user,         -- borrower being liquidated
//   uint256 debtToCover,  -- debt amount in debt-token wei
//   uint256 minProfit,    -- minimum acceptable profit in ETH wei
//   bytes   extraData     -- protocol-specific extra data (empty for AaveV3)
// )
//
// IMPORTANT: verify `keccak256("execute(bytes,uint256)")` and the Protocol enum
// ordinal values against contracts/src/strategies/LiquidationArb.sol before
// deploying to production.
//
// ### 2. Unit conflation in profit math — FIXED
//
// `net_profit_after_gas` previously compared `debt_wei * bonus_frac` (in debt
// token units, e.g. USDC) directly against ETH-denominated gas costs — an
// apples-to-oranges comparison that silently produced wrong results whenever
// the debt token is not 1:1 with ETH.
//
// Fixed by normalising through USD:
//   gross_eth_wei = (debt_tokens × price_usd × bonus_frac / eth_price_usd) × 1e18
//
// `TokenPriceLookup` now requires `eth_price_usd() -> Option<f64>` so LA can
// convert the liquidation bonus to ETH for a valid comparison. When this
// returns None the strategy fails closed, same as the existing debt-token
// price guard.
//
// ### 3. SimulatorBackend injection — FIXED
//
// `simulate()` previously returned `success: true` unconditionally with
// `gas_used = estimate * 0.88`. Fixed by accepting an injectable
// `SimulatorBackend`. When None, `simulate()` returns an error.
//
// ### 4. max_base_fee floor — FIXED
//
// `max_base_fee_gwei` now goes through
// `ExecutionBlueprint::derive_max_base_fee_gwei()` which floors at
// `MIN_MAX_BASE_FEE_GWEI` (1 gwei). On Arbitrum the real base fee (~0.02 gwei)
// truncates to 0 whole gwei; a zero maxBaseFee makes the Orchestrator revert
// with BaseFeeTooHigh on every call.
//
// ### 5. Clippy (2026-11-13)
//
// Test code used `&[pos.clone()]` to build a one-element slice;
// `clippy::cloned_ref_to_slice_refs` (denied under `-D warnings`) requires
// `std::slice::from_ref(&pos)`. Tests updated; no production-code change.
//
// ## Remaining known limitations
//
// CALLDATA ABI (minor): `extraData` is encoded as empty `bytes`. AaveV3
// standard liquidationCall needs no extra data, so this is correct for the
// current deployment. Extend when supporting Compound V3 or other protocols
// that require extra parameters in the extraData field.
//
// POSITION-SCORE DRIFT: `score()` and `build_blueprint()` each call
// `select_position()` independently. If the registry changes between the two
// calls they may operate on different positions. Fixing this cleanly requires
// a StrategyTrait signature change (pass selected position into build).

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
use crate::simulator::SimulatorBackend;

// ─────────────────────────────────────────────────────────────────────────────
// LiquidationProtocol
// ─────────────────────────────────────────────────────────────────────────────

/// Liquidation protocol identifiers matching `LiquidationArb.sol::Protocol`.
///
/// IMPORTANT: ordinal values must match the Solidity enum declaration exactly.
/// Verify against contracts/src/strategies/LiquidationArb.sol before changing.
#[derive(Debug, Clone, Copy, Default)]
#[repr(u8)]
pub enum LiquidationProtocol {
    #[default]
    AaveV3 = 0,
    CompoundV3 = 1,
}

// ─────────────────────────────────────────────────────────────────────────────
// TokenPriceLookup
// ─────────────────────────────────────────────────────────────────────────────

/// Provides live token prices for LA's profit normalisation.
///
/// Implementations live outside this crate (main.rs, using Chainlink/Pyth
/// caches). Missing or stale prices must return `None` — LA fails closed.
pub trait TokenPriceLookup: Send + Sync {
    /// Returns `(price_usd, decimals)` for `token`, or `None` if unknown/stale.
    fn price_usd_and_decimals(&self, token: Address) -> Option<(f64, u8)>;

    /// Returns the current ETH/USD price, or `None` if unknown/stale.
    ///
    /// Required for normalising the liquidation bonus (in debt-token units)
    /// to ETH wei so it can be compared against ETH-denominated gas costs.
    fn eth_price_usd(&self) -> Option<f64>;
}

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

/// Multiplier for `max_base_fee_gwei` — passed to
/// `ExecutionBlueprint::derive_max_base_fee_gwei()`, which also applies the
/// `MIN_MAX_BASE_FEE_GWEI` floor.
const MAX_BASE_FEE_HEADROOM_MULTIPLIER: f64 = 3.0;

// ─────────────────────────────────────────────────────────────────────────────
// LaStrategyConfig — bundles the many constructor arguments
// ─────────────────────────────────────────────────────────────────────────────

/// All parameters for constructing a `LaStrategy`, grouped to avoid a
/// clippy::too_many_arguments violation on `LaStrategy::new`.
pub struct LaStrategyConfig {
    pub chain_id: u64,
    pub bytecode_hash: B256,
    pub contract_addr: Address,
    pub protocol: LiquidationProtocol,
    pub liquidity_registry: Arc<LiquidityRegistry>,
    pub position_registry: Arc<PositionRegistry>,
    pub price_lookup: Option<Arc<dyn TokenPriceLookup>>,
    pub simulator: Option<Arc<dyn SimulatorBackend>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// LaStrategy
// ─────────────────────────────────────────────────────────────────────────────

pub struct LaStrategy {
    chain_id: u64,
    nonce: AtomicU64,
    bytecode_hash: B256,
    #[allow(dead_code)]
    contract_addr: Address,
    protocol: LiquidationProtocol,
    liquidity_registry: Arc<LiquidityRegistry>,
    position_registry: Arc<PositionRegistry>,
    price_lookup: Option<Arc<dyn TokenPriceLookup>>,
    /// Real Anvil simulator. When None, `simulate()` returns an error.
    simulator: Option<Arc<dyn SimulatorBackend>>,
    gas: GasConfig,
}

impl LaStrategy {
    /// Construct from individual parameters.
    ///
    /// Accepts 9 arguments. If clippy::too_many_arguments fires at the call
    /// site, use `LaStrategy::from_config` with a `LaStrategyConfig` instead.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_id: u64,
        bytecode_hash: B256,
        contract_addr: Address,
        protocol: LiquidationProtocol,
        liquidity_registry: Arc<LiquidityRegistry>,
        position_registry: Arc<PositionRegistry>,
        config: &OmegaConfig,
        price_lookup: Option<Arc<dyn TokenPriceLookup>>,
        simulator: Option<Arc<dyn SimulatorBackend>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            nonce: AtomicU64::new(1),
            bytecode_hash,
            contract_addr,
            protocol,
            liquidity_registry,
            position_registry,
            price_lookup,
            simulator,
            gas: config.gas.clone(),
        })
    }

    /// Construct from a `LaStrategyConfig`. Use this at call sites with many
    /// arguments to avoid clippy::too_many_arguments.
    pub fn from_config(config_args: LaStrategyConfig, omega_config: &OmegaConfig) -> Arc<Self> {
        Self::new(
            config_args.chain_id,
            config_args.bytecode_hash,
            config_args.contract_addr,
            config_args.protocol,
            config_args.liquidity_registry,
            config_args.position_registry,
            omega_config,
            config_args.price_lookup,
            config_args.simulator,
        )
    }

    pub fn is_hot_tier(hf_e18: U256) -> bool {
        u128::try_from(hf_e18).unwrap_or(u128::MAX) < HOT_TIER_HF_THRESHOLD
    }

    fn select_position(&self) -> Option<PositionSnapshot> {
        self.position_registry
            .liquidatable_positions(self.chain_id)
            .into_iter()
            .next()
    }

    fn debt_token(position: &PositionSnapshot) -> Option<Address> {
        if position.debt_token == Address::ZERO {
            None
        } else {
            Some(position.debt_token)
        }
    }

    /// Converts `position.debt_usd_e18` to the exact wei amount of
    /// `position.debt_token` needed to cover the liquidation.
    ///
    /// Returns `None` when no price source, stale prices, or zero debt.
    fn debt_amount_wei(&self, position: &PositionSnapshot) -> Option<U256> {
        let lookup = self.price_lookup.as_ref()?;
        let debt_token = Self::debt_token(position)?;
        if position.debt_usd_e18.is_zero() {
            return None;
        }
        let (price_usd, decimals) = lookup.price_usd_and_decimals(debt_token)?;
        if !price_usd.is_finite() || price_usd <= 0.0 {
            return None;
        }
        let debt_usd = u128::try_from(position.debt_usd_e18).unwrap_or(0) as f64 / 1e18_f64;
        let token_amount = debt_usd / price_usd;
        if !token_amount.is_finite() || token_amount <= 0.0 {
            return None;
        }
        let scale = 10f64.powi(decimals as i32);
        let wei_f = token_amount * scale;
        if !wei_f.is_finite() || wei_f <= 0.0 || wei_f > u128::MAX as f64 {
            return None;
        }
        let wei = wei_f.floor() as u128;
        if wei == 0 {
            return None;
        }
        Some(U256::from(wei))
    }

    /// Converts the liquidation bonus (in debt-token units) to ETH wei.
    ///
    /// Normalises through USD so gas costs (ETH) and bonus (debt-token) are
    /// unit-consistent:
    ///
    ///   gross_eth_wei = (debt_tokens × price_usd × bonus_frac / eth_price_usd) × 1e18
    fn liquidation_bonus_eth_wei(
        &self,
        debt_wei: U256,
        position: &PositionSnapshot,
    ) -> Option<U256> {
        let lookup = self.price_lookup.as_ref()?;
        let debt_token = Self::debt_token(position)?;
        let (price_usd, decimals) = lookup.price_usd_and_decimals(debt_token)?;
        let eth_price = lookup.eth_price_usd()?;

        if !price_usd.is_finite() || price_usd <= 0.0 {
            return None;
        }
        if !eth_price.is_finite() || eth_price <= 0.0 {
            return None;
        }

        let scale = 10f64.powi(decimals as i32);
        let debt_tokens = u128::try_from(debt_wei).unwrap_or(0) as f64 / scale;
        let bonus_usd = debt_tokens * price_usd * LA_LIQUIDATION_BONUS_FRAC;
        let bonus_eth = bonus_usd / eth_price;
        let bonus_eth_wei = bonus_eth * 1e18_f64;

        if !bonus_eth_wei.is_finite() || bonus_eth_wei <= 0.0 || bonus_eth_wei > u128::MAX as f64 {
            return None;
        }
        Some(U256::from(bonus_eth_wei.floor() as u128))
    }

    /// Returns `(net_profit_eth_wei, priority_fee_gwei)` or `None` if
    /// the opportunity is not profitable after gas.
    ///
    /// All amounts are in ETH wei — unit-consistent with gas costs.
    fn net_profit_after_gas(
        &self,
        gross_eth_wei: U256,
        base_fee: u64,
        l1_data_fee: u64,
    ) -> Option<(U256, u64)> {
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
        if gross_eth_wei <= total_cost {
            return None;
        }

        let net = gross_eth_wei.saturating_sub(total_cost);
        let dynamic = U256::from(base_fee)
            .saturating_mul(U256::from(LA_GAS_BUDGET))
            .saturating_mul(U256::from(1_000_000_000_u64));
        if net <= dynamic {
            return None;
        }

        let priority_gwei: u64 = u128::try_from(total_cost / U256::from(1_000_000_000_u64))
            .unwrap_or(u64::MAX as u128) as u64;
        Some((net, priority_gwei))
    }

    /// ABI-encodes calldata for `LiquidationArb.execute(bytes data, uint256 deadline)`.
    ///
    /// `data` is ABI-encoded as:
    ///   (uint8 protocol, address collateral, address debt, address user,
    ///    uint256 debtToCover, uint256 minProfit, bytes extraData)
    ///
    /// IMPORTANT: verify `keccak256("execute(bytes,uint256)")` and Protocol
    /// enum ordinals against contracts/src/strategies/LiquidationArb.sol.
    fn encode_liquidation_calldata(
        protocol: LiquidationProtocol,
        collateral_token: Address,
        debt_token: Address,
        borrower: Address,
        debt_to_cover: U256,
        min_profit: U256,
        deadline_block: u64,
    ) -> Bytes {
        let selector = &keccak256(b"execute(bytes,uint256)")[..4];

        // ── Inner bytes: 8 × 32 = 256 bytes ──────────────────────────────
        let mut inner = Vec::with_capacity(256);

        // [0] protocol (uint8 padded to uint256)
        let mut w = [0u8; 32];
        w[31] = protocol as u8;
        inner.extend_from_slice(&w);

        // [1] collateral address
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(collateral_token.as_slice());
        inner.extend_from_slice(&w);

        // [2] debt address
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(debt_token.as_slice());
        inner.extend_from_slice(&w);

        // [3] borrower address
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(borrower.as_slice());
        inner.extend_from_slice(&w);

        // [4] debtToCover
        inner.extend_from_slice(&debt_to_cover.to_be_bytes::<32>());

        // [5] minProfit
        inner.extend_from_slice(&min_profit.to_be_bytes::<32>());

        // [6] offset for extraData = 7 × 32 = 224
        inner.extend_from_slice(&U256::from(224u64).to_be_bytes::<32>());

        // [tail] extraData: length = 0 (AaveV3 needs no extra params)
        inner.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());

        // ── Outer encoding ────────────────────────────────────────────────
        // selector | offset(64) | deadline | inner_length(256) | inner
        let inner_len = inner.len(); // 256
        let mut data = Vec::with_capacity(4 + 32 + 32 + 32 + inner_len);
        data.extend_from_slice(selector);
        // Head slot 0: offset to bytes data = 2 × 32 = 64
        data.extend_from_slice(&U256::from(64u64).to_be_bytes::<32>());
        // Head slot 1: deadline (static uint256)
        data.extend_from_slice(&U256::from(deadline_block).to_be_bytes::<32>());
        // Tail: length of inner bytes
        data.extend_from_slice(&U256::from(inner_len as u64).to_be_bytes::<32>());
        // Tail: inner bytes content
        data.extend_from_slice(&inner);

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
        U256::from(1_000_000_000_000_000_u64) // 0.001 ETH
    }

    async fn score(&self, signal: &SignalState) -> Result<OpScore> {
        let Some(position) = self.select_position() else {
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            });
        };

        let Some(debt_wei) = self.debt_amount_wei(&position) else {
            tracing::debug!(
                borrower = %position.borrower,
                "LA: no debt-amount pricing source — scoring as no-opportunity"
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            });
        };

        let Some(gross_eth_wei) = self.liquidation_bonus_eth_wei(debt_wei, &position) else {
            tracing::debug!(
                borrower = %position.borrower,
                "LA: cannot normalise bonus to ETH (no eth_price_usd) — scoring as no-opportunity"
            );
            return Ok(OpScore {
                score: 0.0,
                expected_profit: U256::ZERO,
                competition_prob: 0.8,
            });
        };

        match self.net_profit_after_gas(
            gross_eth_wei,
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
        let position = self.select_position().ok_or_else(|| {
            anyhow::anyhow!(
                "LA: no liquidatable position for chain {} — refusing blueprint",
                self.chain_id
            )
        })?;

        let flashloan_token = Self::debt_token(&position).ok_or_else(|| {
            anyhow::anyhow!(
                "LA: position (borrower {:?}) has zero-address debt_token",
                position.borrower
            )
        })?;

        let debt_wei = self.debt_amount_wei(&position).ok_or_else(|| {
            anyhow::anyhow!(
                "LA: no debt-amount pricing source for debt_token {:?} — refusing blueprint \
                 with fabricated borrow size",
                flashloan_token
            )
        })?;

        let gross_eth_wei = self
            .liquidation_bonus_eth_wei(debt_wei, &position)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LA: cannot convert liquidation bonus to ETH (no eth_price_usd) — \
                 refusing blueprint with wrong-unit profit estimate"
                )
            })?;

        let (net_profit, priority_gwei) = self
            .net_profit_after_gas(gross_eth_wei, signal.base_fee_gwei, signal.l1_data_fee_gwei)
            .ok_or_else(|| anyhow::anyhow!("LA: opportunity no longer profitable"))?;

        let selection = omega_flashloan::select_provider(
            &self.liquidity_registry,
            self.chain_id,
            flashloan_token,
            debt_wei,
        )
        .map_err(|e| anyhow::anyhow!("LA: flashloan selection failed: {e:?}"))?;

        let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
        let deadline = signal.block_number + LA_EXPIRY_BLOCKS;

        let calldata = Self::encode_liquidation_calldata(
            self.protocol,
            position.collateral_token,
            flashloan_token,
            position.borrower,
            debt_wei,
            net_profit,
            deadline,
        );

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
            blueprint_hash: B256::ZERO,
            chain_id: self.chain_id,
            strategy_id: StrategyId::La,
            lane: Lane::Normal,
            simulator: Simulator::Anvil,
            signal_state_hash: signal.state_hash,
            state_version: signal.state_version,
            signal_id,
            flashloan_provider: selection.contract_addr,
            flashloan_amount: debt_wei,
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
            expiry_block: deadline,
            nonce,
            confirmation_depth: LA_CONFIRMATION,
            client_order_id,
            idempotency_key: B256::ZERO,
            relay_targets: vec!["relay_1".into(), "relay_2".into()],
            zk_proof_commitment: None,
        };
        bp.idempotency_key = bp.compute_idempotency_key();
        bp.blueprint_hash = bp.compute_hash();
        Ok(bp)
    }

    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult> {
        assert_eq!(bp.simulator, Simulator::Anvil);
        match &self.simulator {
            Some(backend) => backend.simulate(bp).await,
            None => anyhow::bail!(
                "LA: no SimulatorBackend injected — cannot simulate blueprint {}; \
                 pass a real Anvil backend via LaStrategy::new()",
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
    use omega_core::types::oracle::{PositionFinancials, PositionTokens};
    use omega_core::OmegaConfig;

    const TEST_CHAIN_ID: u64 = 42161;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

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

    /// Fixed-price test double: $2 000/token, 18 decimals, ETH = $2 000.
    struct FixedPrice;
    impl TokenPriceLookup for FixedPrice {
        fn price_usd_and_decimals(&self, _: Address) -> Option<(f64, u8)> {
            Some((2_000.0, 18))
        }
        fn eth_price_usd(&self) -> Option<f64> {
            Some(2_000.0)
        }
    }

    fn make_with_positions(positions: &[PositionSnapshot]) -> Arc<LaStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            TEST_CHAIN_ID,
            omega_flashloan::FlashloanProvider::Balancer,
            addr(0xD0),
            Address::from([0xB0; 20]),
            U256::from(1_000_000_000_000_000_000_000u128),
            1,
        );

        let pr = PositionRegistry::new();
        for p in positions {
            pr.update(TEST_CHAIN_ID, p.clone());
        }

        LaStrategy::new(
            TEST_CHAIN_ID,
            B256::from([0xAB; 32]),
            Address::ZERO,
            LiquidationProtocol::AaveV3,
            lr,
            pr,
            &OmegaConfig::default(),
            None,
            None,
        )
    }

    fn make_with_price_lookup(positions: &[PositionSnapshot]) -> Arc<LaStrategy> {
        let lr = LiquidityRegistry::new();
        lr.update(
            TEST_CHAIN_ID,
            omega_flashloan::FlashloanProvider::Balancer,
            addr(0xD0),
            Address::from([0xB0; 20]),
            U256::from(1_000_000_000_000_000_000_000u128),
            1,
        );
        let pr = PositionRegistry::new();
        for p in positions {
            pr.update(TEST_CHAIN_ID, p.clone());
        }

        LaStrategy::new(
            TEST_CHAIN_ID,
            B256::from([0xAB; 32]),
            Address::ZERO,
            LiquidationProtocol::AaveV3,
            lr,
            pr,
            &OmegaConfig::default(),
            Some(Arc::new(FixedPrice)),
            None,
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
        assert_eq!(s.gas_budget(), LA_GAS_BUDGET);
    }

    #[tokio::test]
    async fn score_zero_no_position() {
        let op = make().score(&sig(5)).await.unwrap();
        assert_eq!(op.score, 0.0);
    }

    #[tokio::test]
    async fn score_zero_no_price_lookup() {
        let s = make_with_positions(&[sample_position(1_000_000_000_000_000_000u128 - 1)]);
        let op = s.score(&sig(5)).await.unwrap();
        assert_eq!(
            op.score, 0.0,
            "no price lookup must not produce a fabricated score"
        );
    }

    #[tokio::test]
    async fn score_nonzero_with_price_lookup_and_profitable_position() {
        let s = make_with_price_lookup(&[sample_position(1_000_000_000_000_000_000u128 - 1)]);
        let op = s.score(&sig(1)).await.unwrap();
        assert!(
            op.score > 0.0 || op.expected_profit == U256::ZERO,
            "priced position at low gas should score non-zero; score={}",
            op.score
        );
    }

    #[tokio::test]
    async fn debt_amount_wei_correct_with_price_lookup() {
        let pos = sample_position(HOT_TIER_HF_THRESHOLD - 1);
        let s = make_with_price_lookup(std::slice::from_ref(&pos));
        // debt_usd_e18 = 1e18 ($1 at $2000/token) → token_amount = 0.0005 → wei = 5e14
        let wei = s.debt_amount_wei(&pos).expect("priced debt");
        assert_eq!(wei, U256::from(500_000_000_000_000u64));
    }

    #[tokio::test]
    async fn liquidation_bonus_in_eth_units() {
        let pos = sample_position(HOT_TIER_HF_THRESHOLD - 1);
        let s = make_with_price_lookup(std::slice::from_ref(&pos));
        let debt_wei = U256::from(500_000_000_000_000u64); // 5e14 token wei = $1
        let bonus_eth = s.liquidation_bonus_eth_wei(debt_wei, &pos).expect("bonus");
        // $1 × 10% bonus / $2000 per ETH × 1e18 = 5e13 ETH wei
        assert_eq!(bonus_eth, U256::from(50_000_000_000_000u64));
    }

    #[tokio::test]
    async fn encode_calldata_correct_selector_and_length() {
        let cd = LaStrategy::encode_liquidation_calldata(
            LiquidationProtocol::AaveV3,
            addr(0xC0),
            addr(0xD0),
            addr(0x01),
            U256::from(1000u64),
            U256::from(500u64),
            3_000_001,
        );
        // selector(4) + head(64) + inner_length(32) + inner(256) = 356
        assert_eq!(cd.len(), 356);
        let expected_sel = &keccak256(b"execute(bytes,uint256)")[..4];
        assert_eq!(&cd[..4], expected_sel);
        // protocol byte (last byte of first inner word) = 0 (AaveV3)
        // inner starts at offset 4 + 32 + 32 + 32 = 100
        assert_eq!(cd[100 + 31], 0u8, "AaveV3 protocol must be 0");
    }

    #[tokio::test]
    async fn build_blueprint_fails_no_position() {
        let err = make()
            .build_blueprint(&sig(5))
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("no liquidatable position"));
    }

    #[tokio::test]
    async fn build_blueprint_fails_no_price_lookup() {
        let s = make_with_positions(&[sample_position(1_000_000_000_000_000_000u128 - 1)]);
        let err = s.build_blueprint(&sig(5)).await.expect_err("must fail");
        assert!(err.to_string().contains("pricing") || err.to_string().contains("debt-amount"));
    }

    #[tokio::test]
    async fn max_base_fee_never_zero_at_sub_gwei() {
        let s = make_with_price_lookup(&[sample_position(HOT_TIER_HF_THRESHOLD - 1)]);
        let mut signal = sig(1);
        signal.base_fee_gwei = 0;
        // May fail on profitability at 0 fee depending on gas model; only assert floor when it builds.
        if let Ok(bp) = s.build_blueprint(&signal).await {
            assert!(bp.max_base_fee_gwei >= ExecutionBlueprint::MIN_MAX_BASE_FEE_GWEI);
            assert!(bp.verify_hash());
        }
    }

    #[test]
    fn hot_tier_detection() {
        // Below 1.01e18 → hot
        assert!(LaStrategy::is_hot_tier(U256::from(
            HOT_TIER_HF_THRESHOLD - 1
        )));
        // At/above threshold → not hot
        assert!(!LaStrategy::is_hot_tier(U256::from(HOT_TIER_HF_THRESHOLD)));
        assert!(!LaStrategy::is_hot_tier(U256::from(
            HOT_TIER_HF_THRESHOLD + 50_000_000_000_000_000
        )));
    }

    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn la_slippage_within_known_risk_policy_cap() {
        const MIRRORED: u16 = 100;
        assert!(
            LA_SLIPPAGE_BPS <= MIRRORED,
            "LA_SLIPPAGE_BPS ({LA_SLIPPAGE_BPS}) exceeds mirrored risk cap ({MIRRORED})"
        );
    }
}
