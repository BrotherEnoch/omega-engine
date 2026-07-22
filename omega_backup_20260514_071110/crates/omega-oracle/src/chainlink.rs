// crates/omega-oracle/src/chainlink.rs
//
// Chainlink on-chain price feed reader (primary oracle source).
//
// ## Architecture
//
//   A `ChainlinkOracle` holds a DashMap of per-token price cache entries.
//   It is updated by the block subscription task: on every new block it
//   checks which feeds need refreshing (stale or first-read) and fires
//   batched read requests through the RPC rate limiter.
//
//   In the full system, `ChainlinkOracle::fetch` is called by
//   `PerChainOracle::update_prices_for_block` once per block.
//
// ## Staleness
//
//   Chainlink heartbeat is typically 1 hour for price-stable assets and
//   20 minutes for volatile assets.  The feed reports `updatedAt`; we
//   compute age as `now - updatedAt` in seconds.
//
//   The resolution layer rejects any price older than PRIMARY_STALE_SECS
//   (45s) â€” well within Chainlink's heartbeat window for normal markets.
//   During extreme volatility Chainlink triggers outside the heartbeat on
//   any deviation > 0.5%, so staleness beyond 45s is a genuine signal
//   quality issue.
//
// ## Feed registry
//
//   Feed addresses are read from a static registry keyed by (token, chain_id).
//   In production this registry is populated from the Chainlink feed registry
//   contract or a config TOML.  Here we provide Arbitrum mainnet addresses
//   for the tokens used by LA (Â§11).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use dashmap::DashMap;

use crate::resolution::{OraclePrice, OracleSource, PRIMARY_STALE_SECS};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Known Chainlink feed addresses on Arbitrum One (chain 42161)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Map of token symbol â†’ Chainlink aggregator address on Arbitrum One.
///
/// Source: https://docs.chain.link/data-feeds/price-feeds/addresses/?network=arbitrum
pub fn arbitrum_feeds() -> &'static [(&'static str, &'static str)] {
    &[
        ("WETH", "0x639Fe6ab55C921f74e7fac1ee960C0B6293ba612"),
        ("WBTC", "0x6ce185960625439572af5E015ba3cfB1f14Eaba9"),
        ("LINK", "0x86E53CF1B873786aC51Ac36aC8538E84E0Da64C7"),
        ("ARB",  "0xb2A824043730FE05F3DA2efaFa1CBbe83fa548D6"),
        ("USDC", "0x50834F3163758fcC1Df9973b6e91f0F0F0434aD3"),
        ("USDT", "0x3f3f5dF88dC9F13eac63DF89EC16ef6e7E25DdE7"),
    ]
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cached entry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Debug, Clone)]
struct CacheEntry {
    price_usd:    f64,
    updated_at:   u64,   // Unix timestamp (seconds) of on-chain updatedAt
    block_number: u64,
}

impl CacheEntry {
    fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.updated_at)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ChainlinkOracle
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-chain Chainlink price cache.
///
/// Shared across tasks via `Arc<ChainlinkOracle>`.  The cache is updated
/// by the background block-subscription task and read by the resolution layer.
#[derive(Debug)]
pub struct ChainlinkOracle {
    chain_id: u64,
    /// Token symbol â†’ cached price entry.
    cache:    DashMap<String, CacheEntry>,
}

impl ChainlinkOracle {
    pub fn new(chain_id: u64) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            cache: DashMap::new(),
        })
    }

    /// Update the cached price for `token` from an on-chain read result.
    ///
    /// Called by the block-update task when a fresh `latestRoundData`
    /// response is received from the RPC layer.
    ///
    /// `price_usd`:   price in USD (decoded from the aggregator's answer
    ///                scaled by the feed's decimals).
    /// `updated_at`:  Unix timestamp from the aggregator's `updatedAt` field.
    /// `block_number`: block at which the read was performed.
    pub fn update(&self, token: &str, price_usd: f64, updated_at: u64, block_number: u64) {
        self.cache.insert(
            token.to_owned(),
            CacheEntry { price_usd, updated_at, block_number },
        );

        tracing::debug!(
            chain_id     = self.chain_id,
            token,
            price_usd,
            age_secs     = CacheEntry { price_usd, updated_at, block_number }.age_secs(),
            "Chainlink price updated",
        );
    }

    /// Read the cached price for `token`, returning an `OraclePrice`.
    ///
    /// Returns `None` when no entry exists (first block after startup).
    pub fn read(&self, token: &str) -> Option<OraclePrice> {
        let entry = self.cache.get(token)?;
        Some(OraclePrice {
            price_usd:    entry.price_usd,
            source:       OracleSource::Chainlink,
            age_secs:     entry.age_secs(),
            block_number: entry.block_number,
            is_fallback:  false,
        })
    }

    /// Returns `true` when the cached price for `token` is stale.
    ///
    /// Used by the block-update task to decide whether to fire a refresh
    /// RPC read.
    pub fn is_stale(&self, token: &str) -> bool {
        match self.cache.get(token) {
            Some(e) => e.age_secs() >= PRIMARY_STALE_SECS,
            None    => true,  // no cache entry â†’ always stale
        }
    }

    /// Chain ID this oracle instance serves.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_secs() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn update_and_read_fresh_price() {
        let oracle = ChainlinkOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        let p = oracle.read("WETH").unwrap();
        assert!((p.price_usd - 1800.0).abs() < 1e-6);
        assert!(p.age_secs < 5, "just updated should be <5s old");
        assert!(!p.is_fallback);
    }

    #[test]
    fn read_missing_returns_none() {
        let oracle = ChainlinkOracle::new(42161);
        assert!(oracle.read("NOTOKEN").is_none());
    }

    #[test]
    fn missing_entry_is_stale() {
        let oracle = ChainlinkOracle::new(42161);
        assert!(oracle.is_stale("WETH"), "no entry â†’ always stale");
    }

    #[test]
    fn fresh_entry_not_stale() {
        let oracle = ChainlinkOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        assert!(!oracle.is_stale("WETH"), "just updated â†’ not stale");
    }

    #[test]
    fn old_entry_is_stale() {
        let oracle = ChainlinkOracle::new(42161);
        // updated_at far in the past
        oracle.update("WETH", 1800.0, now_secs() - 60, 999_000);
        assert!(oracle.is_stale("WETH"), "60s old â†’ stale at 45s threshold");
    }

    #[test]
    fn overwrite_updates_price() {
        let oracle = ChainlinkOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        oracle.update("WETH", 1850.0, now_secs(), 1_000_001);
        let p = oracle.read("WETH").unwrap();
        assert!((p.price_usd - 1850.0).abs() < 1e-6);
    }

    #[test]
    fn multiple_tokens_independent() {
        let oracle = ChainlinkOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        oracle.update("WBTC", 45_000.0, now_secs(), 1_000_000);
        assert!((oracle.read("WETH").unwrap().price_usd - 1800.0).abs() < 1e-6);
        assert!((oracle.read("WBTC").unwrap().price_usd - 45_000.0).abs() < 1e-6);
    }
}