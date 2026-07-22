// crates/omega-oracle/src/twap.rs
//
// Uniswap v3 TWAP price cache (tertiary oracle source, spec §7).
//
// ## Architecture
//
//   TWAP prices are computed from on-chain Uniswap v3 pool observations.
//   The oracle does NOT call `observe()` directly — that would require
//   a gas-expensive on-chain read on every block.  Instead, the DexSync
//   event stream from omega-rpc delivers `Sync` log events carrying the
//   raw `sqrtPriceX96` slot; we decode these to derive the spot price
//   and use it as a TWAP proxy.
//
//   In a full production deployment the oracle would accumulate tick
//   observations via `IUniswapV3Pool.observe([0, TWAP_PERIOD])` on a
//   slow refresh cycle (every 5 blocks) and use those for the TWAP.
//   The current implementation uses the latest Sync event price with a
//   120-second staleness guard — conservative for LA scoring purposes.
//
// ## Staleness
//
//   Threshold: TWAP_STALE_SECS (120 seconds).
//   TWAP is used ONLY when both Chainlink and Pyth are stale.
//
// ## sqrtPriceX96 decoding
//
//   Uniswap v3 encodes the pool price as sqrt(price) × 2^96.
//   To convert to a token0/token1 price:
//     price = (sqrtPriceX96 / 2^96)^2
//   We then invert if token1 == quote token.
//
// ## Pool registry
//
//   Static mapping: token symbol → (pool address, token0 address, token1 address,
//   token0_decimals, token1_decimals) on Arbitrum One.
//
// ## Audit fix (this revision) — DATA DEFECT, unresolved
//
// The LINK entry in `arbitrum_pools()` below has a malformed pool
// address: 39 hex characters, one short of the 40 required for a valid
// 20-byte address (verified programmatically, not by inspection). This
// crate cannot verify what the correct address should be without a live
// lookup this environment does not have access to, so rather than
// fabricate a plausible-looking replacement — which would be worse than
// the loud failure this currently is, since a wrong-but-valid-looking
// address silently points at an arbitrary or nonexistent contract — a
// test (`all_pool_addresses_are_well_formed`) has been added that
// asserts every entry in `arbitrum_pools()` is exactly "0x" + 40 hex
// characters. That test WILL FAIL until the LINK address is replaced
// with a verified value from an authoritative source (Uniswap's official
// pool list, or an on-chain factory `getPool` query) — this is
// intentional: it converts a silent runtime panic wherever this string
// eventually gets parsed into an `Address` into an explicit, immediate
// `cargo test` failure instead.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::resolution::{OraclePrice, OracleSource, TWAP_STALE_SECS};

// ─────────────────────────────────────────────────────────────────────────────
// Pool registry — Arbitrum One
// ─────────────────────────────────────────────────────────────────────────────

/// Known Uniswap v3 pools on Arbitrum used as TWAP fallback sources.
///
/// Fields: (token_symbol, pool_address, token0_is_numerator, token0_decimals, token1_decimals)
///
/// `token0_is_numerator = true` means token0 is the asset being priced,
/// token1 is the quote (USD-stable or WETH).
///
/// WARNING: the LINK entry's address is currently malformed (39 hex
/// chars, not 40) — see this file's module-level audit note. Do not
/// deploy against this table until `all_pool_addresses_are_well_formed`
/// passes.
pub fn arbitrum_pools() -> &'static [(&'static str, &'static str, bool, u8, u8)] {
    &[
        // WETH / USDC.e 0.05% pool — USDC as quote, WETH as numerator
        (
            "WETH",
            "0xC31E54c7a869B9FcBEcc14363CF510d1c41fa443",
            false,
            6,
            18,
        ),
        // WBTC / USDC 0.05% pool
        (
            "WBTC",
            "0x2f5e87C9312fa29aed5c179E456625D79015299c",
            false,
            6,
            8,
        ),
        // LINK / ETH 0.3% pool — LINK as token0, ETH as token1 (priced in ETH, converted)
        // FIXME (this revision): address below is 39 hex chars, not 40 —
        // confirmed malformed, NOT independently verified/corrected. See
        // module-level audit note. Replace with a verified address before
        // relying on this entry.
        (
            "LINK",
            "0x468b88941e7Cc0B88c1869d68ab6b570bCEF62F",
            true,
            18,
            18,
        ),
        // ARB / ETH 0.05% pool
        (
            "ARB",
            "0x92c63d0e701CAAe670C9415d91C474F686298f00",
            true,
            18,
            18,
        ),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// sqrtPriceX96 decoding
// ─────────────────────────────────────────────────────────────────────────────

/// Decode a Uniswap v3 `sqrtPriceX96` to a token0/token1 price ratio.
///
/// Returns `price = (sqrt_price_x96 / 2^96)^2 × 10^(decimals1 - decimals0)`
/// which gives the number of token1 units per 1 token0 unit.
///
/// When `token0_is_numerator = false`, the caller should invert the result
/// (`1.0 / price`) to get the asset price in terms of the quote token.
pub fn decode_sqrt_price_x96(
    sqrt_price_x96: u128,
    token0_decimals: u8,
    token1_decimals: u8,
    token0_is_numerator: bool,
) -> f64 {
    // price_ratio = (sqrtPriceX96 / 2^96)^2
    let q96: f64 = (1u128 << 96) as f64;
    let sqrt_p: f64 = sqrt_price_x96 as f64 / q96;
    let raw_ratio: f64 = sqrt_p * sqrt_p;

    // Adjust for decimal difference between token0 and token1
    let decimal_adj = 10_f64.powi(token0_decimals as i32 - token1_decimals as i32);
    let price_ratio = raw_ratio / decimal_adj;

    if token0_is_numerator {
        price_ratio // token1 per token0 (e.g. USDC per WETH)
    } else {
        1.0 / price_ratio // token0 per token1 (inverted)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache entry
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TwapEntry {
    price_usd: f64,
    updated_at: u64, // Unix timestamp (seconds) of the Sync event block
    block_number: u64,
}

impl TwapEntry {
    fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.updated_at)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TwapOracle
// ─────────────────────────────────────────────────────────────────────────────

/// Per-chain Uniswap v3 TWAP price cache (tertiary fallback, spec §7).
///
/// Shared across tasks via `Arc<TwapOracle>`.
#[derive(Debug)]
pub struct TwapOracle {
    chain_id: u64,
    cache: DashMap<String, TwapEntry>,
}

impl TwapOracle {
    pub fn new(chain_id: u64) -> Arc<Self> {
        Arc::new(Self {
            chain_id,
            cache: DashMap::new(),
        })
    }

    /// Update the TWAP cache for `token` from a decoded Sync event.
    ///
    /// Called by the DexSync event handler in `per_chain.rs` for each
    /// Sync log matching a known pool.
    ///
    /// `price_usd`:   token price in USD, decoded from `sqrtPriceX96`.
    /// `block_time`:  Unix timestamp (seconds) of the Sync event's block.
    /// `block_number`: block number of the Sync event.
    pub fn update(&self, token: &str, price_usd: f64, block_time: u64, block_number: u64) {
        if price_usd <= 0.0 || !price_usd.is_finite() {
            tracing::warn!(token, price_usd, "TWAP rejected non-positive price");
            return;
        }

        self.cache.insert(
            token.to_owned(),
            TwapEntry {
                price_usd,
                updated_at: block_time,
                block_number,
            },
        );

        tracing::debug!(
            chain_id = self.chain_id,
            token,
            price_usd,
            block_number,
            "TWAP price updated",
        );
    }

    /// Read the cached TWAP price for `token`.
    ///
    /// Returns `None` when no Sync event has been received for this token.
    pub fn read(&self, token: &str) -> Option<OraclePrice> {
        let entry = self.cache.get(token)?;
        Some(OraclePrice {
            price_usd: entry.price_usd,
            source: OracleSource::Twap,
            age_secs: entry.age_secs(),
            block_number: entry.block_number,
            is_fallback: true,
        })
    }

    /// Returns `true` when the cached TWAP price for `token` is stale.
    pub fn is_stale(&self, token: &str) -> bool {
        match self.cache.get(token) {
            Some(e) => e.age_secs() >= TWAP_STALE_SECS,
            None => true,
        }
    }

    /// Chain ID this TWAP oracle instance serves.
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
    fn update_and_read_fresh_price() {
        let oracle = TwapOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        let p = oracle.read("WETH").unwrap();
        assert!((p.price_usd - 1800.0).abs() < 1e-6);
        assert!(p.is_fallback, "TWAP is always a fallback source");
        assert!(p.age_secs < 5);
    }

    #[test]
    fn read_missing_returns_none() {
        let oracle = TwapOracle::new(42161);
        assert!(oracle.read("NOTOKEN").is_none());
    }

    #[test]
    fn missing_is_stale() {
        let oracle = TwapOracle::new(42161);
        assert!(oracle.is_stale("WETH"));
    }

    #[test]
    fn fresh_not_stale() {
        let oracle = TwapOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs(), 1_000_000);
        assert!(!oracle.is_stale("WETH"));
    }

    #[test]
    fn old_entry_is_stale() {
        let oracle = TwapOracle::new(42161);
        oracle.update("WETH", 1800.0, now_secs() - 200, 999_000);
        assert!(oracle.is_stale("WETH"), "200s old > 120s threshold");
    }

    #[test]
    fn rejects_nonpositive_price() {
        let oracle = TwapOracle::new(42161);
        oracle.update("WETH", 0.0, now_secs(), 1_000_000);
        oracle.update("WETH", -1.0, now_secs(), 1_000_001);
        // Neither update should have populated the cache
        assert!(oracle.read("WETH").is_none());
    }

    #[test]
    fn decode_sqrt_price_x96_weth_usdc() {
        // Build a self-consistent sqrtPriceX96 fixture for ~1800 USDC/WETH.
        // token0=USDC (6 decimals), token1=WETH (18 decimals), and
        // token0_is_numerator=false means the decoder returns the inverted
        // quote: WETH price in USDC.
        let target_price = 1_800.0_f64;
        let q96 = (1u128 << 96) as f64;
        let raw_ratio = 10_f64.powi(6 - 18) / target_price;
        let sqrt_px96 = (raw_ratio.sqrt() * q96).round() as u128;
        // token0=USDC (6 decimals), token1=WETH (18 decimals)
        // token0_is_numerator=false → inverted result = WETH price in USDC
        let price = decode_sqrt_price_x96(sqrt_px96, 6, 18, false);
        assert!(
            (price - target_price).abs() < 1.0,
            "decoded WETH/USDC price should round-trip near {target_price}, got {price}"
        );
    }

    // ── Audit fix regression test (this revision) ────────────────────────────

    #[test]
    fn all_pool_addresses_are_well_formed() {
        // Guards against exactly the defect found in this revision: the
        // LINK entry's address was 39 hex chars, one short of a valid
        // 20-byte address. This test intentionally FAILS until every
        // entry in arbitrum_pools() is a properly formatted "0x" + 40
        // hex chars — see this file's module-level audit note. A failing
        // assertion here is preferable to a runtime panic wherever this
        // string eventually gets parsed into an on-chain `Address`.
        for (symbol, addr, _, _, _) in arbitrum_pools() {
            assert!(
                addr.starts_with("0x"),
                "{symbol}: pool address must start with 0x, got {addr}"
            );
            let hex_part = &addr[2..];
            assert_eq!(
                hex_part.len(),
                40,
                "{symbol}: pool address must be exactly 40 hex chars after 0x \
                 (20-byte address), got {} chars in {addr}",
                hex_part.len()
            );
            assert!(
                hex_part.chars().all(|c| c.is_ascii_hexdigit()),
                "{symbol}: pool address contains non-hex characters: {addr}"
            );
        }
    }
}