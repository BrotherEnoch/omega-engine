ï»¿// crates/omega-flashloan/src/lib.rs
//
// omega-flashloan â€” Flashloan provider registry and premium calculator (spec Â§11).
//
// ## Overview
//
//   LA blueprints source capital via flash loans when the required
//   liquidation amount exceeds on-hand capital.  This crate:
//
//   1. Tracks per-provider liquidity availability (`LiquidityRegistry`).
//   2. Calculates the flash loan premium for each provider (`premium_wei`).
//   3. Selects the cheapest available provider for a given amount and
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
//   `LIQUIDITY_STALE_SECS` are not used â€” the selector falls through
//   to the next provider rather than using stale data.
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Constants
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Aave v3 flash loan premium in basis points (9 bps = 0.09%).
pub const AAVE_V3_PREMIUM_BPS: u32 = 9;

/// Balancer flash loan premium in basis points (0 = free).
pub const BALANCER_PREMIUM_BPS: u32 = 0;

/// Uniswap v3 flash loan fee in basis points (30 bps = 0.3% on the
/// 30bps pool tier â€” the most liquid tier for most assets).
pub const UNISWAP_V3_PREMIUM_BPS: u32 = 30;

/// Liquidity snapshot older than this many seconds is treated as stale.
/// The selector skips stale providers rather than using outdated data.
pub const LIQUIDITY_STALE_SECS: i64 = 30;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// FlashloanProvider
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Identifies a flash loan provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashloanProvider {
    /// Aave v3 Pool â€” 9 bps premium.  Primary provider for LA on Arbitrum.
    AaveV3,
    /// Balancer Vault â€” 0 bps premium.  Preferred when available.
    Balancer,
    /// Uniswap v3 Pool â€” 30 bps fee.  Last resort for assets not on Aave/Balancer.
    UniswapV3,
}

impl FlashloanProvider {
    /// Flash loan premium in basis points.
    #[inline]
    pub fn premium_bps(self) -> u32 {
        match self {
            FlashloanProvider::AaveV3    => AAVE_V3_PREMIUM_BPS,
            FlashloanProvider::Balancer  => BALANCER_PREMIUM_BPS,
            FlashloanProvider::UniswapV3 => UNISWAP_V3_PREMIUM_BPS,
        }
    }

    /// Selection priority: lower = preferred.
    /// Balancer(0) > AaveV3(1) > UniswapV3(2).
    #[inline]
    pub fn priority(self) -> u8 {
        match self {
            FlashloanProvider::Balancer  => 0,
            FlashloanProvider::AaveV3    => 1,
            FlashloanProvider::UniswapV3 => 2,
        }
    }

    /// Canonical display name for telemetry labels.
    pub fn as_str(self) -> &'static str {
        match self {
            FlashloanProvider::AaveV3    => "aave_v3",
            FlashloanProvider::Balancer  => "balancer",
            FlashloanProvider::UniswapV3 => "uniswap_v3",
        }
    }
}

impl std::fmt::Display for FlashloanProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// FlashloanError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FlashloanError {
    /// No registered provider has sufficient liquidity for `amount_wei`
    /// on `chain_id`.  Records the best available amount for diagnostics.
    #[error(
        "No flash loan provider available for {amount_wei} wei on chain \
         {chain_id}: best available = {best_available_wei} wei"
    )]
    NoneAvailable {
        amount_wei:          U256,
        chain_id:            u64,
        best_available_wei:  U256,
    },

    /// The requested amount exceeds this specific provider's liquidity.
    #[error(
        "Provider {provider} has {available_wei} wei available; \
         requested {requested_wei} wei"
    )]
    InsufficientLiquidity {
        provider:       FlashloanProvider,
        available_wei:  U256,
        requested_wei:  U256,
    },

    /// The liquidity snapshot is too old to be trusted.
    #[error("Provider {provider} liquidity snapshot is stale ({age_secs}s > {threshold_secs}s)")]
    StaleSnapshot {
        provider:        FlashloanProvider,
        age_secs:        i64,
        threshold_secs:  i64,
    },
}

impl FlashloanError {
    /// Map to the canonical `OmegaError` for pipeline drop tracking.
    pub fn to_omega_error(&self) -> OmegaError {
        OmegaError::dropped(DropCode::MissFlashloan)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LiquiditySnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A point-in-time liquidity reading from a flash loan provider.
///
/// Updated by omega-oracle on every relevant Supply/Withdraw/Borrow event.
/// Read synchronously by `LiquidityRegistry::available` in the hot path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquiditySnapshot {
    /// Available flash loan liquidity in wei.
    pub available_wei:  U256,
    /// Block number at which this snapshot was taken.
    pub block_number:   u64,
    /// UTC timestamp when the snapshot was recorded.
    pub recorded_at:    DateTime<Utc>,
}

impl LiquiditySnapshot {
    /// Returns `true` when this snapshot is fresh enough to be trusted.
    pub fn is_fresh(&self) -> bool {
        let age = Utc::now().signed_duration_since(self.recorded_at).num_seconds();
        age <= LIQUIDITY_STALE_SECS
    }

    /// Age of this snapshot in seconds.
    pub fn age_secs(&self) -> i64 {
        Utc::now().signed_duration_since(self.recorded_at).num_seconds()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ProviderKey
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Composite key for the liquidity registry: (chain_id, provider, contract_addr).
///
/// Multiple pools of the same provider type can co-exist on a chain
/// (e.g. multiple Uniswap v3 pools with different fee tiers).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderKey {
    chain_id:  u64,
    provider:  FlashloanProvider,
    contract:  Address,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LiquidityRegistry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Concurrent map of per-provider, per-chain liquidity availability.
///
/// Written by omega-oracle background tasks; read synchronously by the
/// blueprint construction path.  All reads are O(1) and lock-free via
/// DashMap.
///
/// Shared via `Arc<LiquidityRegistry>`.
#[derive(Debug)]
pub struct LiquidityRegistry {
    /// (chain_id, provider, contract) â†’ snapshot
    snapshots: DashMap<ProviderKey, LiquiditySnapshot>,
}

impl LiquidityRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshots: DashMap::new(),
        })
    }

    /// Record a fresh liquidity snapshot.
    ///
    /// Called by omega-oracle after processing a Supply/Withdraw/Borrow event
    /// or after a periodic full-sync poll.
    pub fn update(
        &self,
        chain_id:      u64,
        provider:      FlashloanProvider,
        contract:      Address,
        available_wei: U256,
        block_number:  u64,
    ) {
        let key = ProviderKey { chain_id, provider, contract };
        let snap = LiquiditySnapshot {
            available_wei,
            block_number,
            recorded_at: Utc::now(),
        };
        self.snapshots.insert(key, snap);

        tracing::debug!(
            provider      = %provider,
            chain_id,
            available_eth = format!("{:.6}", u256_to_eth(available_wei)),
            block_number,
            "Flashloan liquidity updated",
        );
    }

    /// Return the freshest liquidity snapshot for a (chain, provider, contract)
    /// triplet.
    ///
    /// Returns `None` when no snapshot exists or the snapshot is stale.
    pub fn snapshot(
        &self,
        chain_id:  u64,
        provider:  FlashloanProvider,
        contract:  Address,
    ) -> Option<LiquiditySnapshot> {
        let key  = ProviderKey { chain_id, provider, contract };
        let snap = self.snapshots.get(&key)?.clone();
        if snap.is_fresh() { Some(snap) } else { None }
    }

    /// All registered contracts for a given (chain_id, provider), with fresh
    /// snapshots, sorted descending by available liquidity.
    pub fn available_contracts(
        &self,
        chain_id: u64,
        provider: FlashloanProvider,
    ) -> Vec<(Address, LiquiditySnapshot)> {
        let mut entries: Vec<(Address, LiquiditySnapshot)> = self.snapshots
            .iter()
            .filter(|e| e.key().chain_id == chain_id && e.key().provider == provider)
            .filter(|e| e.value().is_fresh())
            .map(|e| (e.key().contract, e.value().clone()))
            .collect();

        // Highest liquidity first â€” prefer deep pools
        entries.sort_by(|a, b| b.1.available_wei.cmp(&a.1.available_wei));
        entries
    }
}

impl Default for LiquidityRegistry {
    fn default() -> Self {
        Self { snapshots: DashMap::new() }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Premium calculation
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute the flash loan premium cost in wei.
///
/// `premium_wei = amount_wei Ã— premium_bps / 10_000`
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
    // amount Ã— bps / 10_000
    amount_wei
        .saturating_mul(U256::from(provider.premium_bps()))
        / U256::from(10_000_u32)
}

/// Total repayment amount (principal + premium) in wei.
#[inline]
pub fn repayment_wei(provider: FlashloanProvider, amount_wei: U256) -> U256 {
    amount_wei.saturating_add(premium_wei(provider, amount_wei))
}

/// Convert wei â†’ ETH as f64 for human-readable telemetry/logging.
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SelectionResult
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// The result of `select_provider` â€” the cheapest available provider that
/// can fill the requested amount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    /// The selected provider.
    pub provider:      FlashloanProvider,
    /// The contract address to call for the flash loan.
    pub contract_addr: Address,
    /// Premium cost in wei.
    pub premium_wei:   U256,
    /// Total repayment (principal + premium) in wei.
    pub repayment_wei: U256,
    /// Available liquidity at selection time (may exceed `amount_wei`).
    pub available_wei: U256,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Provider selector
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Select the cheapest flash loan provider that can fill `amount_wei` on
/// `chain_id`.
///
/// ## Selection algorithm
///
///   1. For each provider in priority order (Balancer â†’ AaveV3 â†’ UniswapV3):
///      a. Query `registry.available_contracts(chain_id, provider)`.
///      b. Take the first contract with `available_wei â‰¥ amount_wei`.
///      c. Return `SelectionResult` for that contract.
///   2. If no provider can fill, return `Err(FlashloanError::NoneAvailable)`.
///
/// The caller (strategy blueprint construction) uses the `contract_addr`
/// to populate `blueprint.flashloan_provider` and `premium_wei` to
/// compute `blueprint.expected_profit_net`.
pub fn select_provider(
    registry:   &LiquidityRegistry,
    chain_id:   u64,
    amount_wei: U256,
) -> Result<SelectionResult, FlashloanError> {
    let providers = [
        FlashloanProvider::Balancer,
        FlashloanProvider::AaveV3,
        FlashloanProvider::UniswapV3,
    ];

    let mut best_available = U256::ZERO;

    for provider in providers {
        let contracts = registry.available_contracts(chain_id, provider);

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
                    amount_eth    = format!("{:.6}", u256_to_eth(amount_wei)),
                    premium_eth   = format!("{:.9}", u256_to_eth(prem)),
                    available_eth = format!("{:.6}", u256_to_eth(snap.available_wei)),
                    "Flash loan provider selected",
                );

                return Ok(SelectionResult {
                    provider,
                    contract_addr: *contract,
                    premium_wei:   prem,
                    repayment_wei: repay,
                    available_wei: snap.available_wei,
                });
            }
        }
    }

    Err(FlashloanError::NoneAvailable {
        amount_wei,
        chain_id,
        best_available_wei: best_available,
    })
}

// Calldata encoding lives in encoding.rs to keep this file under 600 lines.
pub use encoding::encode_flashloan_call;

#[cfg(test)]
mod tests;