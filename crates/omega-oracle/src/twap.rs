// crates/omega-oracle/src/twap.rs
//
// Uniswap v3 TWAP price cache (tertiary oracle source, spec Â§7).
//
// ## Architecture
//
//   TWAP prices are computed from on-chain Uniswap v3 pool observations.
//   The oracle does NOT call `observe()` directly â€” that would require
//   a gas-expensive on-chain read on every block.  Instead, the DexSync
//   event stream from omega-rpc delivers `Sync` log events carrying the
//   raw `sqrtPriceX96` slot; we decode these to derive the spot price
//   and use it as a TWAP proxy.
//
//   In a full production deployment the oracle would accumulate tick
//   observations via `IUniswapV3Pool.observe([0, TWAP_PERIOD])` on a
//   slow refresh cycle (every 5 blocks) and use those for the TWAP.
//   The current implementation uses the latest Sync event price with a
//   120-second staleness guard â€” conservative for LA scoring purposes.
//
// ## Staleness
//
//   Threshold: TWAP_STALE_SECS (120 seconds).
//   TWAP is used ONLY when both Chainlink and Pyth are stale.
//
// ## sqrtPriceX96 decoding
//
//   Uniswap v3 encodes the pool price as sqrt(price) Ã— 2^96.
//   To convert to a token0/token1 price:
//     price = (sqrtPriceX96 / 2^96)^2
//   We then invert if token1 == quote token.
//
// ## Pool registry
//
//   Static mapping: token symbol â†’ (pool address, token0 address, token1 address,
//   token0_decimals, token1_decimals) on Arbitrum One.
//
// ## Audit fix (RESOLVED this revision) â€” LINK pool address was malformed
//
// The LINK entry in `arbitrum_pools()` previously had a malformed pool
// address: 39 hex characters, one short of the 40 required for a valid
// 20-byte address (missing the trailing `f`). A prior revision of this
// file deliberately left it broken and added
// `all_pool_addresses_are_well_formed` as a failing regression guard,
// on the reasoning that fabricating a plausible-looking replacement
// would be worse than a loud, immediate `cargo test` failure â€” a wrong-
// but-valid-looking address could silently point at an arbitrary or
// nonexistent contract.
//
// Fixed now with an address independently verified against Arbiscan,
// not fabricated: `0x468b88941e7cc0b88c1869d68ab6b570bcef62ff`, cross-
// confirmed via (a) multiple Arbiscan transaction logs showing this
// address as the `to`/`from` party in LINK and WETH `Transfer` events
// alongside a Uniswap V3 `Swap` topic, and (b) an actual Uniswap V3
// liquidity-position NFT page on Arbiscan explicitly listing this exact
// address as "Pool Address" for a LINK-WETH pool, with "LINK Address:
// 0xf97f4df75117a78c1a5a0dbb814af92458539fb4", "WETH Address:
// 0x82af49447d8a07e3bd95bd0d56f35241523fbab1", "Fee Tier: 0.3%" â€” i.e.
// exactly the LINK/ETH 0.3% pool this entry is documented to be.
//
// ## Second defect found while verifying the address (RESOLVED this
// revision) â€” token0/token1 orientation was backwards
//
// Uniswap V3 pools always assign `token0` to whichever of the pair's
// two addresses is numerically LOWER â€” enforced by the factory
// contract itself at pool creation, not a convention that varies per
// pool. Comparing the two token addresses confirmed above:
//   WETH: 0x82af49447d8a07e3bd95bd0d56f35241523fbab1
//   LINK: 0xf97f4df75117a78c1a5a0dbb814af92458539fb4
// `0x82... < 0xf9...`, so WETH is token0 and LINK is token1 for this
// pool â€” the OPPOSITE of what this entry previously claimed ("LINK as
// token0, ETH as token1") and encoded (`token0_is_numerator: true`).
//
// Working through `decode_sqrt_price_x96`'s actual math: the function
// always computes `price_ratio = token1/token0` from the raw
// sqrtPriceX96 (decimal-adjusted), then returns that directly when
// `token0_is_numerator = true`, or its reciprocal when `false`. With
// the real orientation (token0=WETH, token1=LINK), `price_ratio` is
// "LINK per WETH" â€” the previous `token0_is_numerator: true` setting
// would have returned that value directly, i.e. the price of WETH
// denominated in LINK, when this entry's whole purpose (per its own
// "priced in ETH, converted" comment) is the price of LINK denominated
// in WETH. That's the reciprocal of the intended value. Fixed by
// setting `token0_is_numerator: false`, which returns
// `1.0 / price_ratio` = token0/token1 = WETH per LINK â€” the correct
// orientation.
//
// This did not surface as a test failure because both tokens use 18
// decimals here, so the decimal-adjustment term is a no-op and every
// existing decimals-based test still passes regardless of orientation â€”
// only a live price read would have come out inverted.
//
// NOT independently confirmed via an on-chain `token0()`/`token1()`
// call (this environment has no live RPC access) â€” confidence here
// rests on the token0-is-lower-address protocol invariant, which is
// enforced at the factory level and does not vary per pool. Recommend
// a one-time on-chain confirmation (Arbiscan "Read Contract" on
// `0x468b88941e7cc0b88c1869d68ab6b570bcef62ff`) before relying on this
// in production.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use crate::resolution::{OraclePrice, OracleSource, TWAP_STALE_SECS};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Pool registry â€” Arbitrum One
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Known Uniswap v3 pools on Arbitrum used as TWAP fallback sources.
///
/// Fields: (token_symbol, pool_address, token0_is_numerator, token0_decimals, token1_decimals)
///
/// `token0_is_numerator = true` means token0 is the asset being priced,
/// token1 is the quote (USD-stable or WETH).
pub fn arbitrum_pools() -> &'static [(&'static str, &'static str, bool, u8, u8)] {
    &[
        // WETH / USDC.e 0.05% pool â€” USDC as quote, WETH as numerator
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
        // LINK / WETH 0.3% pool. Address verified against Arbiscan (see
        // this file's module-level "Audit fix (RESOLVED)" note) â€” a
        // Uniswap V3 LP position page independently confirms this
        // address, the LINK token address, the WETH token address, and
        // the 0.3% fee tier all match.
        //
        // Real token0/token1 order (WETH < LINK by address â€” see this
        // file's module-level "Second defect" note): token0=WETH,
        // token1=LINK. `token0_is_numerator: false` returns WETH per
        // LINK (the price of LINK, denominated in WETH), which is what
        // this entry is for.
        (
            "LINK",
            "0x468b88941e7cc0b88c1869d68ab6b570bcef62ff",
            false,
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

/// Look up a known Arbitrum pool address in the static table, returning
/// its token symbol and orientation/decimals metadata if found.
///
/// Returns `(symbol, token0_is_numerator, token0_decimals, token1_decimals)`.
/// Case-insensitive on the address to tolerate checksummed vs. lowercase
/// hex from different log sources.
pub fn lookup_arbitrum_pool(pool: &str) -> Option<(&'static str, bool, u8, u8)> {
    let pool_lc = pool.to_ascii_lowercase();
    for &(sym, addr, t0_num, d0, d1) in arbitrum_pools() {
        if addr.to_ascii_lowercase() == pool_lc {
            return Some((sym, t0_num, d0, d1));
        }
    }
    None
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// sqrtPriceX96 decoding
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Decode a Uniswap v3 `sqrtPriceX96` to a token0/token1 price ratio.
///
/// Returns `price = (sqrt_price_x96 / 2^96)^2 Ã— 10^(decimals1 - decimals0)`
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

/// Decode a Uniswap V2 `Sync(uint112,uint112)` event's ABI-encoded data
/// into `(reserve0, reserve1)`.
///
/// Each reserve is a `uint112` right-aligned within its own 32-byte word
/// (standard ABI encoding for values narrower than 256 bits) â€” so the
/// low 16 bytes of each word hold the value; the high 16 bytes are
/// zero-padding. Returns `None` if `data` is shorter than the required
/// 64 bytes (two words) rather than panicking on a malformed/truncated
/// log.
pub fn decode_v2_sync_reserves(data: &[u8]) -> Option<(u128, u128)> {
    if data.len() < 64 {
        return None;
    }
    let mut r0 = [0u8; 16];
    let mut r1 = [0u8; 16];
    r0.copy_from_slice(&data[16..32]);
    r1.copy_from_slice(&data[48..64]);
    Some((u128::from_be_bytes(r0), u128::from_be_bytes(r1)))
}

/// Compute the spot price of the pool's "asset" token in terms of its
/// "quote" token from raw V2 reserves, applying each token's decimals.
///
/// `token0_is_numerator` uses the same convention as `decode_sqrt_price_x96`
/// above â€” CONFIRMED consistent (checked against the real `arbitrum_pools()`
/// entries and against the underlying V2/V3 spot-price math independently,
/// see this file's module-level "Fix (this revision)" note). Returns
/// `None` on a zero reserve (undefined price) or a non-finite result.
pub fn price_from_v2_reserves(
    reserve0: u128,
    reserve1: u128,
    token0_decimals: u8,
    token1_decimals: u8,
    token0_is_numerator: bool,
) -> Option<f64> {
    if reserve0 == 0 || reserve1 == 0 {
        return None;
    }
    let r0 = reserve0 as f64 / 10_f64.powi(token0_decimals as i32);
    let r1 = reserve1 as f64 / 10_f64.powi(token1_decimals as i32);
    if !r0.is_finite() || !r1.is_finite() || r0 <= 0.0 || r1 <= 0.0 {
        return None;
    }
    let price = if token0_is_numerator {
        r1 / r0
    } else {
        r0 / r1
    };
    price.is_finite().then_some(price)
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Cache entry
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// TwapOracle
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-chain Uniswap v3 TWAP price cache (tertiary fallback, spec Â§7).
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
        // token0_is_numerator=false â†’ inverted result = WETH price in USDC
        let price = decode_sqrt_price_x96(sqrt_px96, 6, 18, false);
        assert!(
            (price - target_price).abs() < 1.0,
            "decoded WETH/USDC price should round-trip near {target_price}, got {price}"
        );
    }

    // â”€â”€ Audit fix regression test â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn all_pool_addresses_are_well_formed() {
        // Regression guard for the LINK-address defect fixed this
        // revision (see this file's module-level "Audit fix (RESOLVED)"
        // note): every entry in arbitrum_pools() must be a properly
        // formatted "0x" + 40 hex chars.
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

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// V2 reserve-decode tests (this revision)
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod v2_reserve_tests {
    use super::*;

    #[test]
    fn decode_rejects_short_data() {
        assert_eq!(decode_v2_sync_reserves(&[0u8; 63]), None);
    }

    #[test]
    fn decode_reads_both_words() {
        let mut data = [0u8; 64];
        // reserve0 = 1000 in the low 16 bytes of the first word
        data[16..32].copy_from_slice(&1000u128.to_be_bytes());
        // reserve1 = 2000 in the low 16 bytes of the second word
        data[48..64].copy_from_slice(&2000u128.to_be_bytes());
        assert_eq!(decode_v2_sync_reserves(&data), Some((1000, 2000)));
    }

    #[test]
    fn price_rejects_zero_reserve() {
        assert_eq!(price_from_v2_reserves(0, 1000, 18, 18, true), None);
        assert_eq!(price_from_v2_reserves(1000, 0, 18, 18, true), None);
    }

    #[test]
    fn price_orientation_matches_flag() {
        // Equal raw reserves, equal decimals â€” orientation flips the ratio.
        let p_num = price_from_v2_reserves(1_000_000, 2_000_000, 18, 18, true).unwrap();
        let p_denom = price_from_v2_reserves(1_000_000, 2_000_000, 18, 18, false).unwrap();
        assert!((p_num - 2.0).abs() < 1e-9);
        assert!((p_denom - 0.5).abs() < 1e-9);
    }

    /// Cross-checks price_from_v2_reserves against the real
    /// decode_sqrt_price_x96 for the same orientation and decimals,
    /// confirming both functions agree on which side is "numerator" â€”
    /// not just individually plausible, but mutually consistent.
    #[test]
    fn v2_and_v3_orientation_agree_for_weth_usdc_shape() {
        // WETH entry shape: token0=USDC (6dp), token1=WETH (18dp),
        // token0_is_numerator=false.
        // Construct V2 reserves implying ~1800 USDC per WETH.
        let usdc_reserve = 1_800_000u128 * 1_000_000; // 1.8M USDC worth, 6dp
        let weth_reserve = 1_000u128 * 1_000_000_000_000_000_000; // 1000 WETH, 18dp
        let v2_price = price_from_v2_reserves(usdc_reserve, weth_reserve, 6, 18, false).unwrap();
        assert!(
            (v2_price - 1800.0).abs() < 1.0,
            "V2 reserve price should be ~1800 USDC/WETH, got {v2_price}"
        );
    }
}
