// crates/omega-oracle/src/pyth.rs
//
// Pyth Network price feed cache (secondary oracle source).
//
// ## Architecture
//
//   Pyth prices arrive via the Pyth HTTP price service or via on-chain
//   Pyth contracts.  In the Omega pipeline they are delivered through the
//   omega-rpc subscription layer (parsed from block logs or SSE feed)
//   and pushed into this cache by the block-update task.
//
// ## Confidence interval guard
//
//   Pyth publishes a confidence interval (± spread) with each price.
//   We reject any price where confidence / price > MAX_CONFIDENCE_RATIO
//   (default 1% = 100 bps).  A wide confidence interval signals low
//   price certainty — treating such prices as stale prevents trading on
//   uncertain data.
//
// ## Staleness
//
//   Pyth prices carry a `publish_time` Unix timestamp.  Age is computed
//   as `now - publish_time`.  Threshold: PRIMARY_STALE_SECS (45s),
//   same as Chainlink.
//
// ## Price IDs
//
//   Pyth identifies feeds by a 32-byte price ID (not token address).
//   We maintain a static mapping from token symbol → Pyth price ID for
//   the tokens used by LA and SA on Arbitrum.
//
// ## Source
//   https://pyth.network/developers/price-feed-ids — Crypto feeds

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::resolution::{
    validate_observation_timestamp, validate_price_usd, OraclePrice, OracleSource,
    PRIMARY_STALE_SECS,
};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum allowed confidence-to-price ratio before the price is treated
/// as stale (1% = 0.01).
pub const MAX_CONFIDENCE_RATIO: f64 = 0.01;

// ─────────────────────────────────────────────────────────────────────────────
// Pyth price IDs for Arbitrum LA tokens
// ─────────────────────────────────────────────────────────────────────────────

/// Token symbol → Pyth price feed ID (hex without 0x prefix).
pub fn arbitrum_price_ids() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "WETH",
            "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
        ),
        (
            "WBTC",
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
        ),
        (
            "LINK",
            "8ac0c70fff57e9aefdf5edf44b51d62c2d433653cbb2cf5cc06bb115af04d221",
        ),
        (
            "ARB",
            "3fa4252848f9f0a1480be62745a4629d9eb1322aebab8a791e344b3b9c1adcf5",
        ),
        (
            "USDC",
            "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
        ),
        (
            "USDT",
            "2b89b9dc8fdf9f34709a5b106b472f0f39bb6ca9ce04b0fd7f2e971688e2e53b",
        ),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// CacheEntry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CacheEntry {
    /// Price in USD.
    price_usd: f64,
    /// ± confidence in USD.
    confidence_usd: f64,
    /// Unix timestamp (seconds) from Pyth's `publishTime`.
    publish_time: u64,
    /// Block number at which this price was ingested.
    block_number: u64,
}

impl CacheEntry {
    fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.publish_time)
    }

    /// Returns `true` when the confidence interval is within acceptable bounds.
    fn confidence_ok(&self) -> bool {
        if self.price_usd <= 0.0 {
            return false;
        }
        let ratio = self.confidence_usd / self.price_usd;
        ratio <= MAX_CONFIDENCE_RATIO
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PythOracle
// ─────────────────────────────────────────────────────────────────────────────

/// Per-chain Pyth price cache.
///
/// Shared via `Arc<PythOracle>`.  Updated by the block-subscription task;
/// read by the tri-oracle resolution layer.
#[derive(Debug)]
pub struct PythOracle {
    chain_id: u64,
    cache: DashMap<String, CacheEntry>,
}

impl PythOracle {
    pub fn new(chain_id: u64) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            cache: DashMap::new(),
        })
    }

    /// Update the cached price for `token` from a Pyth price update.
    ///
    /// `price_usd`:      mid-price in USD.
    /// `confidence_usd`: ± confidence bound in USD.
    /// `publish_time`:   Unix timestamp (seconds) from the Pyth price
    ///                   attestation.
    /// `block_number`:   block at which this update was ingested.
    pub fn update(
        &self,
        token: &str,
        price_usd: f64,
        confidence_usd: f64,
        publish_time: u64,
        block_number: u64,
    ) {
        // C8 fail-closed: never cache invalid price or missing/future timestamps.
        if let Err(reason) = validate_price_usd(price_usd) {
            tracing::warn!(
                chain_id = self.chain_id,
                token,
                price_usd,
                reason,
                "Pyth update rejected (fail closed) — cache unchanged",
            );
            return;
        }
        if let Err(reason) = validate_observation_timestamp(publish_time) {
            tracing::warn!(
                chain_id = self.chain_id,
                token,
                publish_time,
                reason,
                "Pyth update rejected (fail closed) — cache unchanged",
            );
            return;
        }

        let entry = CacheEntry {
            price_usd,
            confidence_usd,
            publish_time,
            block_number,
        };

        if !entry.confidence_ok() {
            tracing::warn!(
                chain_id = self.chain_id,
                token,
                price_usd,
                confidence_usd,
                ratio = confidence_usd / price_usd.max(1e-12),
                "Pyth: wide confidence interval — treating as stale",
            );
        }

        self.cache.insert(token.to_owned(), entry);

        tracing::debug!(
            chain_id = self.chain_id,
            token,
            price_usd,
            confidence = confidence_usd,
            "Pyth price updated",
        );
    }

    /// Read the cached price for `token`.
    ///
    /// Returns `None` when:
    ///   - No entry exists.
    ///   - The confidence interval is too wide (treated as stale).
    pub fn read(&self, token: &str) -> Option<OraclePrice> {
        let entry = self.cache.get(token)?;

        // Reject wide confidence — treat as unavailable
        if !entry.confidence_ok() {
            tracing::debug!(token, "Pyth: wide confidence interval — read returns None",);
            return None;
        }

        Some(OraclePrice {
            price_usd: entry.price_usd,
            source: OracleSource::Pyth,
            age_secs: entry.age_secs(),
            block_number: entry.block_number,
            is_fallback: true,
        })
    }

    /// Returns `true` when the cached Pyth price is stale or unavailable.
    pub fn is_stale(&self, token: &str) -> bool {
        match self.cache.get(token) {
            Some(e) => !e.confidence_ok() || e.age_secs() >= PRIMARY_STALE_SECS,
            None => true,
        }
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn fresh_tight_confidence_readable() {
        let p = PythOracle::new(42161);
        p.update("WETH", 1800.0, 1.0, now_secs(), 1_000_000);
        let price = p.read("WETH").unwrap();
        assert!((price.price_usd - 1800.0).abs() < 1e-6);
        assert!(price.is_fallback);
        assert!(price.age_secs < 5);
    }

    #[test]
    fn wide_confidence_returns_none() {
        let p = PythOracle::new(42161);
        // confidence = 20 USD on 1800 → 1.1% > 1% threshold
        p.update("WETH", 1800.0, 20.0, now_secs(), 1_000_000);
        assert!(p.read("WETH").is_none(), "wide confidence must return None");
    }

    #[test]
    fn wide_confidence_is_stale() {
        let p = PythOracle::new(42161);
        p.update("WETH", 1800.0, 20.0, now_secs(), 1_000_000);
        assert!(p.is_stale("WETH"));
    }

    #[test]
    fn exact_1pct_confidence_is_acceptable() {
        let p = PythOracle::new(42161);
        // confidence = 18 → 18/1800 = 1.0% exactly ≤ threshold
        p.update("WETH", 1800.0, 18.0, now_secs(), 1_000_000);
        assert!(p.read("WETH").is_some(), "1% confidence must be accepted");
    }

    #[test]
    fn missing_token_is_stale() {
        let p = PythOracle::new(42161);
        assert!(p.is_stale("NOTOKEN"));
    }

    #[test]
    fn stale_price_detected() {
        let p = PythOracle::new(42161);
        // publish_time 60s in the past → stale at 45s threshold
        p.update("WETH", 1800.0, 1.0, now_secs() - 60, 999_000);
        assert!(p.is_stale("WETH"));
    }
}