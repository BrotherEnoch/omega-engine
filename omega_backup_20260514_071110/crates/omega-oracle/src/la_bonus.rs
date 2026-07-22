ï»¿// crates/omega-oracle/src/la_bonus.rs
//
// Per-asset, per-protocol liquidation bonus oracle (spec Â§11, Â§12).
//
// ## Purpose
//
//   The liquidation bonus determines the maximum gross revenue from an LA
//   opportunity.  The adaptive gas cap formula (Â§12.2) uses it to set the
//   priority fee ceiling:
//
//     base_cap = (bonus_eth Ã— 1e9 Ã— 0.05) / GAS_PER_BUNDLE
//
//   An incorrect bonus estimate leads to either under-bidding (lost to
//   competitors) or over-bidding (net loss at the emergency bundle tier).
//
// ## Data sources
//
//   Each protocol encodes the liquidation bonus differently:
//
//   Aave v3:    `ReserveData.liquidationBonus` in basis points above 10000.
//               e.g. 10500 = 5% bonus.  Fetched via `getReserveData`.
//
//   Compound v3: `LiquidationFactor` expressed as a percentage of the
//               collateral absorbed.  Discount ~5â€“8% depending on asset.
//               Fetched via the Comet contract's `getAssetInfo`.
//
//   Morpho Blue: Derived from the market's LLTV.
//               bonus â‰ˆ 1 / LLTV âˆ’ 1 (inverse liquidation value margin).
//               Fetched via `market(id).lltv` and converted.
//
//   Euler v2:   `liquidationDiscount` per vault, expressed in basis points.
//               Fetched via `IEVault.liquidationDiscount`.
//
// ## Update cadence
//
//   Bonus parameters are governance-controlled and change infrequently.
//   The oracle refreshes them:
//   - On startup (full scan)
//   - When a governance event for the relevant protocol is detected in the
//     lending protocol log stream from omega-rpc
//   - On a periodic fallback: every 1 hour
//
// ## Static defaults
//
//   On startup, before the first on-chain read completes, the oracle
//   returns the static defaults from the registry below.  These are
//   conservative estimates verified against current protocol deployments.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256};
use dashmap::DashMap;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Protocol identifier
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LendingProtocol {
    AaveV3,
    CompoundV3,
    Morpho,
    EulerV2,
}

impl std::fmt::Display for LendingProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LendingProtocol::AaveV3     => f.write_str("aave_v3"),
            LendingProtocol::CompoundV3 => f.write_str("compound_v3"),
            LendingProtocol::Morpho     => f.write_str("morpho"),
            LendingProtocol::EulerV2    => f.write_str("euler_v2"),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Static default bonuses (in basis points)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Static default liquidation bonus registry, keyed by (protocol, asset symbol).
///
/// Expressed in basis points.  e.g. 500 = 5% bonus.
/// Source: verified against current deployments on Arbitrum One (April 2026).
fn default_bonus_bps(protocol: LendingProtocol, asset: &str) -> u16 {
    match (protocol, asset) {
        // Aave v3 Arbitrum liquidation bonuses from ReserveData
        (LendingProtocol::AaveV3, "WETH")  => 500,   // 5%
        (LendingProtocol::AaveV3, "WBTC")  => 750,   // 7.5%
        (LendingProtocol::AaveV3, "LINK")  => 750,
        (LendingProtocol::AaveV3, "ARB")   => 1000,  // 10%
        (LendingProtocol::AaveV3, "USDC")  => 500,
        (LendingProtocol::AaveV3, _)       => 750,   // conservative default

        // Compound v3 absorb discounts
        (LendingProtocol::CompoundV3, "WETH") => 500,
        (LendingProtocol::CompoundV3, "WBTC") => 500,
        (LendingProtocol::CompoundV3, _)      => 600,

        // Morpho Blue â€” market-specific; use conservative estimate
        (LendingProtocol::Morpho, _) => 600,

        // Euler v2 â€” vault-specific discount
        (LendingProtocol::EulerV2, "WETH") => 500,
        (LendingProtocol::EulerV2, "WBTC") => 600,
        (LendingProtocol::EulerV2, _)      => 700,
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cache entry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone)]
struct BonusEntry {
    /// Bonus in basis points (e.g. 500 = 5%).
    bonus_bps:    u16,
    /// Whether this came from an on-chain read (`true`) or a static
    /// default (`false`).
    is_live:      bool,
    /// Time of the last update.
    last_updated: Instant,
}

impl BonusEntry {
    fn is_stale(&self, ttl: Duration) -> bool {
        self.last_updated.elapsed() > ttl
    }

    /// Bonus as a fraction (e.g. 500 bps â†’ 0.05).
    fn bonus_fraction(&self) -> f64 {
        self.bonus_bps as f64 / 10_000.0
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cache key
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BonusKey {
    protocol: LendingProtocol,
    /// Asset symbol (e.g. "WETH") or market_id hex for Morpho.
    asset_key: String,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LaBonusOracle
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-chain liquidation bonus oracle.
///
/// Returns bonus percentages in basis points for the adaptive gas cap
/// formula (Â§12.2).  Falls back to static defaults when live data is
/// not yet available.
#[derive(Debug)]
pub struct LaBonusOracle {
    chain_id: u64,
    cache:    DashMap<BonusKey, BonusEntry>,
    /// Live entry TTL â€” entries older than this are considered stale and
    /// will trigger a refresh on the next block update.
    ttl:      Duration,
}

/// How long live bonus entries are trusted before requiring a refresh.
const DEFAULT_BONUS_TTL: Duration = Duration::from_secs(3_600); // 1 hour

impl LaBonusOracle {
    pub fn new(chain_id: u64) -> Arc<Self> {
        Self::with_ttl(chain_id, DEFAULT_BONUS_TTL)
    }

    pub fn with_ttl(chain_id: u64, ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            cache: DashMap::new(),
            ttl,
        })
    }

    // â”€â”€ Write path â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Record a live bonus update from an on-chain read.
    ///
    /// Called by the governance event handler in `per_chain.rs` when a
    /// protocol emits a reserve-configuration change event, or by the
    /// periodic refresh task.
    pub fn update_asset(&self, protocol: LendingProtocol, asset: &str, bonus_bps: u16) {
        let key = BonusKey { protocol, asset_key: asset.to_owned() };
        self.cache.insert(key, BonusEntry {
            bonus_bps,
            is_live:      true,
            last_updated: Instant::now(),
        });

        tracing::debug!(
            chain_id  = self.chain_id,
            protocol  = %protocol,
            asset,
            bonus_bps,
            "Liquidation bonus updated (live)",
        );
    }

    /// Record a live Morpho Blue bonus update from market data.
    ///
    /// Morpho bonuses are keyed by market ID (B256) rather than asset symbol.
    pub fn update_morpho_market(&self, market_id: B256, bonus_bps: u16) {
        let key = BonusKey {
            protocol:  LendingProtocol::Morpho,
            asset_key: hex::encode(market_id.as_slice()),
        };
        self.cache.insert(key, BonusEntry {
            bonus_bps,
            is_live:      true,
            last_updated: Instant::now(),
        });

        tracing::debug!(
            chain_id  = self.chain_id,
            market_id = %market_id,
            bonus_bps,
            "Morpho Blue bonus updated (live)",
        );
    }

    // â”€â”€ Read path â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Aave v3 liquidation bonus for a collateral asset in basis points.
    ///
    /// Returns the live cached value when available and fresh; falls back
    /// to the static default when no live data exists.
    pub fn aave_v3_bonus_bps(&self, asset: &str) -> u16 {
        self.read_or_default(LendingProtocol::AaveV3, asset)
    }

    /// Compound v3 absorb discount for a collateral asset in basis points.
    pub fn compound_v3_bonus_bps(&self, asset: &str) -> u16 {
        self.read_or_default(LendingProtocol::CompoundV3, asset)
    }

    /// Euler v2 liquidation discount for a collateral asset in basis points.
    pub fn euler_v2_bonus_bps(&self, asset: &str) -> u16 {
        self.read_or_default(LendingProtocol::EulerV2, asset)
    }

    /// Morpho Blue bonus for a market, keyed by market ID.
    ///
    /// Falls back to the Morpho-wide static default when the market-specific
    /// value has not been loaded.
    pub fn morpho_blue_bonus_bps(&self, market_id: B256) -> u16 {
        let key = BonusKey {
            protocol:  LendingProtocol::Morpho,
            asset_key: hex::encode(market_id.as_slice()),
        };
        match self.cache.get(&key) {
            Some(e) if !e.is_stale(self.ttl) => e.bonus_bps,
            _ => default_bonus_bps(LendingProtocol::Morpho, ""),
        }
    }

    /// Convenience: bonus fraction (e.g. 500 bps â†’ 0.05) for Aave v3.
    pub fn aave_v3_bonus_fraction(&self, asset: &str) -> f64 {
        self.aave_v3_bonus_bps(asset) as f64 / 10_000.0
    }

    /// Convenience: bonus fraction for Compound v3.
    pub fn compound_v3_bonus_fraction(&self, asset: &str) -> f64 {
        self.compound_v3_bonus_bps(asset) as f64 / 10_000.0
    }

    /// Returns `true` when the entry for `(protocol, asset)` is stale
    /// or missing â€” the background refresh task should re-fetch.
    pub fn needs_refresh(&self, protocol: LendingProtocol, asset: &str) -> bool {
        let key = BonusKey { protocol, asset_key: asset.to_owned() };
        match self.cache.get(&key) {
            Some(e) => e.is_stale(self.ttl),
            None    => true,
        }
    }

    // â”€â”€ Internal â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    fn read_or_default(&self, protocol: LendingProtocol, asset: &str) -> u16 {
        let key = BonusKey { protocol, asset_key: asset.to_owned() };
        match self.cache.get(&key) {
            Some(e) if !e.is_stale(self.ttl) => e.bonus_bps,
            _ => {
                let default = default_bonus_bps(protocol, asset);
                tracing::debug!(
                    chain_id = self.chain_id,
                    protocol = %protocol,
                    asset,
                    default_bps = default,
                    "Using static default bonus (no live data)",
                );
                default
            }
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aave_static_default_weth() {
        let oracle = LaBonusOracle::new(42161);
        assert_eq!(oracle.aave_v3_bonus_bps("WETH"), 500);
        assert!((oracle.aave_v3_bonus_fraction("WETH") - 0.05).abs() < 1e-9);
    }

    #[test]
    fn aave_live_update_overrides_default() {
        let oracle = LaBonusOracle::new(42161);
        oracle.update_asset(LendingProtocol::AaveV3, "WETH", 600);
        assert_eq!(oracle.aave_v3_bonus_bps("WETH"), 600);
    }

    #[test]
    fn compound_default_wbtc() {
        let oracle = LaBonusOracle::new(42161);
        assert_eq!(oracle.compound_v3_bonus_bps("WBTC"), 500);
    }

    #[test]
    fn morpho_live_update() {
        let oracle = LaBonusOracle::new(42161);
        let market = B256::from([0xAB; 32]);
        oracle.update_morpho_market(market, 650);
        assert_eq!(oracle.morpho_blue_bonus_bps(market), 650);
    }

    #[test]
    fn morpho_unknown_market_uses_default() {
        let oracle = LaBonusOracle::new(42161);
        let market = B256::from([0xFF; 32]);
        let bps    = oracle.morpho_blue_bonus_bps(market);
        assert_eq!(bps, default_bonus_bps(LendingProtocol::Morpho, ""));
    }

    #[test]
    fn stale_entry_falls_back_to_default() {
        // Zero TTL â†’ every entry is immediately stale
        let oracle = LaBonusOracle::with_ttl(42161, Duration::from_millis(0));
        oracle.update_asset(LendingProtocol::AaveV3, "WETH", 999);
        // Stale immediately â€” should return default
        assert_eq!(oracle.aave_v3_bonus_bps("WETH"), 500);
    }

    #[test]
    fn needs_refresh_when_missing() {
        let oracle = LaBonusOracle::new(42161);
        assert!(oracle.needs_refresh(LendingProtocol::AaveV3, "WETH"));
    }

    #[test]
    fn needs_refresh_false_when_fresh() {
        let oracle = LaBonusOracle::new(42161);
        oracle.update_asset(LendingProtocol::AaveV3, "WETH", 500);
        assert!(!oracle.needs_refresh(LendingProtocol::AaveV3, "WETH"));
    }

    #[test]
    fn euler_v2_default() {
        let oracle = LaBonusOracle::new(42161);
        assert_eq!(oracle.euler_v2_bonus_bps("WETH"), 500);
        assert_eq!(oracle.euler_v2_bonus_bps("UNKNOWN"), 700);
    }
}