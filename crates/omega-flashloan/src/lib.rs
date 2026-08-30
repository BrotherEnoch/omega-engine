// crates/omega-flashloan/src/lib.rs
//
// omega-flashloan — Flashloan provider registry and premium calculator (spec §11).
//
// ## Overview
//
//   LA blueprints source capital via flash loans when the required
//   liquidation amount exceeds on-hand capital.  This crate:
//
//   1. Tracks per-provider, per-asset liquidity availability (`LiquidityRegistry`).
//   2. Calculates the flash loan premium for each provider (`premium_wei`).
//   3. Selects the cheapest available provider for a given asset, amount, and
//      chain (`select_provider`), returning `Err(FlashloanError::NoneAvailable)`
//      when no provider can fill the request.
//   4. Encodes provider-specific calldata for the flashloan callback ABI.
//
// ## Providers (v12)
//
//   | Provider       | Chain     | Premium     | Max amount  |
//   |----------------|-----------|-------------|-------------|
//   | Aave v3        | Arbitrum  | 9 bps       | Pool depth  |
//   | Balancer       | Arbitrum  | 0 bps       | Vault bal.  |
//   | Uniswap v3     | Arbitrum  | 30 bps      | Pool depth  |
//   | Aave v3        | Ethereum  | 9 bps       | Pool depth  |
//
//   Balancer flash loans are free (0 premium) but require the full amount
//   to be repaid in the same transaction.  They are preferred when
//   available.  Aave v3 is the primary fallback.  Uniswap v3 is last
//   resort (highest premium).
//
// ## Liquidity staleness
//
//   Provider liquidity is updated by omega-oracle from on-chain events
//   (Supply, Withdraw, Borrow events).  Snapshots older than
//   `LIQUIDITY_STALE_SECS` are not used — the selector falls through
//   to the next provider rather than using stale data.
//
// ## CHANGE — asset-scoped registry keys (fixes a real cross-asset overwrite bug)
//
//   `ProviderKey` previously was `(chain_id, provider, contract)` — no asset
//   field. Aave's Pool and Balancer's Vault are each a SINGLE contract that
//   serves every token on that chain, so a second tracked asset (e.g. adding
//   USDC alongside WETH) would have landed at the exact same key as the first
//   and silently overwritten its snapshot — not a panic, not a stale-data
//   warning, just a wrong number for whichever asset lost the race. This was
//   never hit in production because only WETH was ever polled, but it made
//   adding a second asset unsafe without this change.
//
//   `ProviderKey` now includes `asset: Address`, and `LiquidityRegistry::update`,
//   `LiquidityRegistry::snapshot`, `LiquidityRegistry::available_contracts`, and
//   `select_provider` all take an explicit `asset` parameter. There is
//   deliberately no default or "primary asset" concept — every caller must say
//   which token it means, the same posture this crate already takes on
//   Uniswap's `asset_is_token0` in `encoding.rs`.
//
//   `FlashloanError::NoneAvailable` gained a matching `asset: Address` field for
//   the same reason: a "no provider available" log line is not actionable once
//   more than one asset is tracked, if it doesn't say which asset failed.
//
// ## Calldata encoding
//
//   Each provider has a distinct flash loan ABI.  `encode_flashloan_call`
//   returns the ABI-encoded initiation call that the strategy contract
//   must make at the start of execution.  The callback calldata
//   (repayment instructions) is embedded in `blueprint.calldata` by the
//   strategy itself.

pub mod encoding;

use std::sync::Arc;

use alloy_primitives::{Address, U256};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use omega_core::errors::{DropCode, OmegaError};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Aave v3 flash loan premium in basis points (9 bps = 0.09%).
pub const AAVE_V3_PREMIUM_BPS: u32 = 9;

/// Balancer flash loan premium in basis points (0 = free).
pub const BALANCER_PREMIUM_BPS: u32 = 0;

/// Uniswap v3 flash loan fee in basis points (30 bps = 0.3% on the
/// 30bps pool tier — the most liquid tier for most assets).
pub const UNISWAP_V3_PREMIUM_BPS: u32 = 30;

/// Liquidity snapshot older than this many seconds is treated as stale.
/// The selector skips stale providers rather than using outdated data.
pub const LIQUIDITY_STALE_SECS: i64 = 30;

// ─────────────────────────────────────────────────────────────────────────────
// FlashloanProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Identifies a flash loan provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashloanProvider {
    /// Aave v3 Pool — 9 bps premium.  Primary provider for LA on Arbitrum.
    AaveV3,
    /// Balancer Vault — 0 bps premium.  Preferred when available.
    Balancer,
    /// Uniswap v3 Pool — 30 bps fee.  Last resort for assets not on Aave/Balancer.
    UniswapV3,
}

impl FlashloanProvider {
    /// Flash loan premium in basis points.
    #[inline]
    pub fn premium_bps(self) -> u32 {
        match self {
            FlashloanProvider::AaveV3 => AAVE_V3_PREMIUM_BPS,
            FlashloanProvider::Balancer => BALANCER_PREMIUM_BPS,
            FlashloanProvider::UniswapV3 => UNISWAP_V3_PREMIUM_BPS,
        }
    }

    /// Selection priority: lower = preferred.
    /// Balancer(0) > AaveV3(1) > UniswapV3(2).
    #[inline]
    pub fn priority(self) -> u8 {
        match self {
            FlashloanProvider::Balancer => 0,
            FlashloanProvider::AaveV3 => 1,
            FlashloanProvider::UniswapV3 => 2,
        }
    }

    /// Canonical display name for telemetry labels.
    pub fn as_str(self) -> &'static str {
        match self {
            FlashloanProvider::AaveV3 => "aave_v3",
            FlashloanProvider::Balancer => "balancer",
            FlashloanProvider::UniswapV3 => "uniswap_v3",
        }
    }
}

impl std::fmt::Display for FlashloanProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FlashloanError
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlashloanError {
    /// No registered provider has sufficient liquidity for `amount_wei` of
    /// `asset` on `chain_id`.  Records the best available amount for
    /// diagnostics.
    ///
    /// `asset` was added alongside the registry's own asset-scoping (see
    /// this module's top-level "CHANGE" note): before that change, a
    /// `NoneAvailable` error gave no way to tell whether the caller had
    /// asked for WETH, USDC, or something else — in a system tracking a
    /// single asset that was implicit, but with multiple assets tracked it
    /// became a real diagnostic gap (a log line reading "no provider
    /// available for 50000000000000000000 wei" is not actionable on its
    /// own once more than one token is in play).
    #[error(
        "No flash loan provider available for {amount_wei} wei of {asset} on chain \
         {chain_id}: best available = {best_available_wei} wei"
    )]
    NoneAvailable {
        amount_wei: U256,
        chain_id: u64,
        asset: Address,
        best_available_wei: U256,
    },

    /// The requested amount exceeds this specific provider's liquidity.
    #[error(
        "Provider {provider} has {available_wei} wei available; \
         requested {requested_wei} wei"
    )]
    InsufficientLiquidity {
        provider: FlashloanProvider,
        available_wei: U256,
        requested_wei: U256,
    },

    /// The liquidity snapshot is too old to be trusted.
    #[error("Provider {provider} liquidity snapshot is stale ({age_secs}s > {threshold_secs}s)")]
    StaleSnapshot {
        provider: FlashloanProvider,
        age_secs: i64,
        threshold_secs: i64,
    },
}

impl FlashloanError {
    /// Map to the canonical `OmegaError` for pipeline drop tracking.
    pub fn to_omega_error(&self) -> OmegaError {
        OmegaError::dropped(DropCode::MissFlashloan)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LiquiditySnapshot
// ─────────────────────────────────────────────────────────────────────────────

/// A point-in-time liquidity reading from a flash loan provider.
///
/// Updated by omega-oracle on every relevant Supply/Withdraw/Borrow event.
/// Read synchronously by `LiquidityRegistry::available` in the hot path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquiditySnapshot {
    /// Available flash loan liquidity in wei.
    pub available_wei: U256,
    /// Block number at which this snapshot was taken.
    pub block_number: u64,
    /// UTC timestamp when the snapshot was recorded.
    pub recorded_at: DateTime<Utc>,
}

impl LiquiditySnapshot {
    /// Returns `true` when this snapshot is fresh enough to be trusted.
    pub fn is_fresh(&self) -> bool {
        let age = Utc::now()
            .signed_duration_since(self.recorded_at)
            .num_seconds();
        age <= LIQUIDITY_STALE_SECS
    }

    /// Age of this snapshot in seconds.
    pub fn age_secs(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.recorded_at)
            .num_seconds()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ProviderKey
// ─────────────────────────────────────────────────────────────────────────────

/// Composite key for the liquidity registry: (chain_id, provider, asset, contract).
///
/// `asset` was added because Aave's Pool and Balancer's Vault are each a
/// single contract shared across every token they support — without an
/// asset component in the key, tracking a second token at the same
/// provider/contract would silently overwrite the first token's snapshot.
/// See this module's top-level "CHANGE" note for the full history.
///
/// Multiple pools of the same provider type can co-exist on a chain
/// (e.g. multiple Uniswap v3 pools with different fee tiers for the same
/// asset) — `contract` distinguishes those.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderKey {
    chain_id: u64,
    provider: FlashloanProvider,
    asset: Address,
    contract: Address,
}

// ─────────────────────────────────────────────────────────────────────────────
// LiquidityRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Concurrent map of per-provider, per-asset, per-chain liquidity availability.
///
/// Written by omega-oracle background tasks; read synchronously by the
/// blueprint construction path.  All reads are O(1) and lock-free via
/// DashMap.
///
/// Shared via `Arc<LiquidityRegistry>`.
#[derive(Debug)]
pub struct LiquidityRegistry {
    /// (chain_id, provider, asset, contract) → snapshot
    snapshots: DashMap<ProviderKey, LiquiditySnapshot>,
}

impl LiquidityRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshots: DashMap::new(),
        })
    }

    /// Record a fresh liquidity snapshot for a specific `asset` at a
    /// specific `(chain_id, provider, contract)`.
    ///
    /// Called by omega-oracle after processing a Supply/Withdraw/Borrow event
    /// or after a periodic full-sync poll. `asset` must be the actual token
    /// this reading is for — a shared-contract provider (Aave Pool, Balancer
    /// Vault) can and will have multiple, independently-tracked assets at the
    /// same `contract` address; passing the wrong `asset` here silently
    /// corrupts that other asset's snapshot, same risk this key change was
    /// made to eliminate at the type level for callers that get it right.
    /// C9 fail-closed: `asset == Address::ZERO` is refused — a zero asset key is
    /// never a real ERC-20 and would only create a poison registry row that
    /// `select_provider` could never legitimately match for LA debt tokens.
    /// The previous snapshot for any real key is left untouched.
    pub fn update(
        &self,
        chain_id: u64,
        provider: FlashloanProvider,
        asset: Address,
        contract: Address,
        available_wei: U256,
        block_number: u64,
    ) {
        if asset.is_zero() {
            tracing::warn!(
                provider = %provider,
                chain_id,
                contract = %contract,
                "LiquidityRegistry::update refused Address::ZERO asset (C9 fail closed)                  — cache unchanged"
            );
            return;
        }
        if contract.is_zero() {
            tracing::warn!(
                provider = %provider,
                chain_id,
                asset = %asset,
                "LiquidityRegistry::update refused Address::ZERO contract (C9 fail closed)                  — cache unchanged"
            );
            return;
        }

        let key = ProviderKey {
            chain_id,
            provider,
            asset,
            contract,
        };
        let snap = LiquiditySnapshot {
            available_wei,
            block_number,
            recorded_at: Utc::now(),
        };
        self.snapshots.insert(key, snap);

        tracing::debug!(
            provider      = %provider,
            chain_id,
            asset         = %asset,
            available_eth = format!("{:.6}", u256_to_eth(available_wei)),
            block_number,
            "Flashloan liquidity updated",
        );
    }

    /// Return the freshest liquidity snapshot for a (chain, provider, asset,
    /// contract) quadruplet.
    ///
    /// Returns `None` when no snapshot exists or the snapshot is stale.
    pub fn snapshot(
        &self,
        chain_id: u64,
        provider: FlashloanProvider,
        asset: Address,
        contract: Address,
    ) -> Option<LiquiditySnapshot> {
        let key = ProviderKey {
            chain_id,
            provider,
            asset,
            contract,
        };
        let snap = self.snapshots.get(&key)?.clone();
        if snap.is_fresh() {
            Some(snap)
        } else {
            None
        }
    }

    /// All registered contracts for a given (chain_id, provider, asset), with
    /// fresh snapshots, sorted descending by available liquidity.
    ///
    /// `asset` scopes the result to a single token — liquidity for a
    /// different asset at the same contract is never mixed in here.
    pub fn available_contracts(
        &self,
        chain_id: u64,
        provider: FlashloanProvider,
        asset: Address,
    ) -> Vec<(Address, LiquiditySnapshot)> {
        let mut entries: Vec<(Address, LiquiditySnapshot)> = self
            .snapshots
            .iter()
            .filter(|e| {
                e.key().chain_id == chain_id
                    && e.key().provider == provider
                    && e.key().asset == asset
            })
            .filter(|e| e.value().is_fresh())
            .map(|e| (e.key().contract, e.value().clone()))
            .collect();

        // Highest liquidity first — prefer deep pools
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.1.available_wei));
        entries
    }
}

impl Default for LiquidityRegistry {
    fn default() -> Self {
        Self {
            snapshots: DashMap::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Premium calculation
// ─────────────────────────────────────────────────────────────────────────────

/// Compute the flash loan premium cost in wei.
///
/// `premium_wei = amount_wei × premium_bps / 10_000`
///
/// Uses U256 arithmetic to avoid overflow on large positions.
///
/// ## Example
///
/// ```rust
/// use alloy_primitives::U256;
/// use omega_flashloan::{FlashloanProvider, premium_wei};
///
/// let amount = U256::from(10_000_000_000_000_000_000_u128); // 10 ETH
/// let cost   = premium_wei(FlashloanProvider::AaveV3, amount);
/// // 9 bps = 0.09% of 10 ETH = 0.009 ETH = 9_000_000_000_000_000 wei
/// assert_eq!(cost, U256::from(9_000_000_000_000_000_u128));
/// ```
pub fn premium_wei(provider: FlashloanProvider, amount_wei: U256) -> U256 {
    // amount × bps / 10_000
    amount_wei.saturating_mul(U256::from(provider.premium_bps())) / U256::from(10_000_u32)
}

/// Total repayment amount (principal + premium) in wei.
#[inline]
pub fn repayment_wei(provider: FlashloanProvider, amount_wei: U256) -> U256 {
    amount_wei.saturating_add(premium_wei(provider, amount_wei))
}

/// Convert wei → ETH as f64 for human-readable telemetry/logging.
///
/// NOT for financial/accounting logic.
/// Execution logic must always remain in integer wei precision.
#[inline]
fn u256_to_eth(value_wei: U256) -> f64 {
    const WEI_PER_ETH: f64 = 1_000_000_000_000_000_000.0;

    // Safe for telemetry purposes.
    // We intentionally accept floating-point precision loss here because
    // this function is only used for logs/metrics formatting.
    value_wei.to::<u128>() as f64 / WEI_PER_ETH
}

// ─────────────────────────────────────────────────────────────────────────────
// SelectionResult
// ─────────────────────────────────────────────────────────────────────────────

/// The result of `select_provider` — the cheapest available provider that
/// can fill the requested amount of the requested asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    /// The selected provider.
    pub provider: FlashloanProvider,
    /// The contract address to call for the flash loan.
    pub contract_addr: Address,
    /// Premium cost in wei.
    pub premium_wei: U256,
    /// Total repayment (principal + premium) in wei.
    pub repayment_wei: U256,
    /// Available liquidity at selection time (may exceed `amount_wei`).
    pub available_wei: U256,
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider selector
// ─────────────────────────────────────────────────────────────────────────────

/// Select the cheapest flash loan provider that can fill `amount_wei` of
/// `asset` on `chain_id`.
///
/// ## Selection algorithm
///
///   1. For each provider in priority order (Balancer → AaveV3 → UniswapV3):
///      a. Query `registry.available_contracts(chain_id, provider, asset)`.
///      b. Take the first contract with `available_wei ≥ amount_wei`.
///      c. Return `SelectionResult` for that contract.
///   2. If no provider can fill, return `Err(FlashloanError::NoneAvailable)`.
///
/// `asset` is required and not inferred — liquidity for a different token at
/// the same provider contract is never substituted in, by construction of
/// `LiquidityRegistry::available_contracts`'s asset filter.
///
/// The caller (strategy blueprint construction) uses the `contract_addr`
/// to populate `blueprint.flashloan_provider` and `premium_wei` to
/// compute `blueprint.expected_profit_net`.
pub fn select_provider(
    registry: &LiquidityRegistry,
    chain_id: u64,
    asset: Address,
    amount_wei: U256,
) -> Result<SelectionResult, FlashloanError> {
    // C9 fail closed: never select against a zero asset (not a real debt token).
    if asset.is_zero() {
        return Err(FlashloanError::NoneAvailable {
            amount_wei,
            chain_id,
            asset,
            best_available_wei: U256::ZERO,
        });
    }

    let providers = [
        FlashloanProvider::Balancer,
        FlashloanProvider::AaveV3,
        FlashloanProvider::UniswapV3,
    ];

    let mut best_available = U256::ZERO;

    for provider in providers {
        let contracts = registry.available_contracts(chain_id, provider, asset);

        for (contract, snap) in &contracts {
            // Track best available for diagnostics
            if snap.available_wei > best_available {
                best_available = snap.available_wei;
            }

            if snap.available_wei >= amount_wei {
                let prem = premium_wei(provider, amount_wei);
                let repay = repayment_wei(provider, amount_wei);

                tracing::debug!(
                    provider      = %provider,
                    contract      = %contract,
                    asset         = %asset,
                    amount_eth    = format!("{:.6}", u256_to_eth(amount_wei)),
                    premium_eth   = format!("{:.9}", u256_to_eth(prem)),
                    available_eth = format!("{:.6}", u256_to_eth(snap.available_wei)),
                    "Flash loan provider selected",
                );

                return Ok(SelectionResult {
                    provider,
                    contract_addr: *contract,
                    premium_wei: prem,
                    repayment_wei: repay,
                    available_wei: snap.available_wei,
                });
            }
        }
    }

    Err(FlashloanError::NoneAvailable {
        amount_wei,
        chain_id,
        asset,
        best_available_wei: best_available,
    })
}

// Calldata encoding lives in encoding.rs to keep this file under 600 lines.
pub use encoding::encode_flashloan_call;

#[cfg(test)]
mod tests;