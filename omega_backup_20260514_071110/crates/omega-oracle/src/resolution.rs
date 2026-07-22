ï»¿// crates/omega-oracle/src/resolution.rs
//
// Tri-oracle price resolution (spec Â§7).
//
// ## Oracle sources
//
//   Primary:   Chainlink â€” on-chain TWAP aggregator, highest trust.
//              Staleness threshold: 45 seconds.
//   Secondary: Pyth â€” off-chain price aggregation network, low-latency.
//              Staleness threshold: 45 seconds.
//   Tertiary:  Uniswap v3 TWAP â€” on-chain AMM price, adversarial-resistant.
//              Staleness threshold: 120 seconds.
//
// ## Resolution rules (priority order)
//
//   1. Chainlink fresh AND Pyth fresh AND prices agree (< 0.4% divergence)
//      â†’ return Chainlink (highest trust primary).
//
//   2. Chainlink fresh AND Pyth fresh AND prices DIVERGE (â‰¥ 0.4%)
//      â†’ DropCode::MissOracleDiverge â€” both primaries disagree; do not trade.
//
//   3. Chainlink fresh, Pyth stale
//      â†’ return Chainlink (single primary available).
//
//   4. Pyth fresh, Chainlink stale
//      â†’ return Pyth (single primary available).
//
//   5. Both primaries stale AND Uniswap TWAP fresh
//      â†’ return TWAP (tertiary fallback).
//
//   6. All stale
//      â†’ DropCode::MissOracle â€” no price available.
//
// ## Divergence threshold
//
//   0.4% (40 bps) between Chainlink and Pyth.  Tighter than typical
//   market spread â€” divergence above this indicates a data quality issue
//   (one feed lagging, oracle manipulation attempt, or extreme volatility).
//
// ## Why TWAP is never compared against primaries for divergence
//
//   TWAP can lag spot by design (time-weighted average).  Requiring TWAP
//   agreement would reject valid signals during fast markets.  TWAP is
//   used only when both primaries are unavailable.

use std::time::Duration;

use omega_core::errors::{DropCode, OmegaError};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Staleness thresholds
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Chainlink and Pyth maximum age before considered stale.
pub const PRIMARY_STALE_SECS: u64 = 45;

/// Uniswap v3 TWAP maximum age before considered stale.
pub const TWAP_STALE_SECS: u64 = 120;

/// Maximum relative price divergence between Chainlink and Pyth before
/// triggering MissOracleDiverge (0.4% = 40 bps).
pub const DIVERGENCE_THRESHOLD: f64 = 0.004;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OraclePrice
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A resolved price from a single oracle source.
///
/// Prices are in USD with 18 decimal places of precision.  We use f64
/// here for scoring arithmetic; the strategy layer converts to U256 e18
/// before any on-chain calldata encoding.
#[derive(Debug, Clone)]
pub struct OraclePrice {
    /// USD price of 1 token unit.
    pub price_usd: f64,
    /// Which oracle produced this price.
    pub source: OracleSource,
    /// How many seconds ago this price was observed on-chain.
    pub age_secs: u64,
    /// Block number at which the price was observed.
    pub block_number: u64,
    /// Whether this price comes from a fallback path (Pyth, TWAP)
    /// rather than the primary Chainlink feed.
    pub is_fallback: bool,
}

impl OraclePrice {
    /// Returns `true` when this price is fresh enough to be trusted for
    /// the given source type.
    pub fn is_fresh(&self) -> bool {
        let threshold = match self.source {
            OracleSource::Chainlink => PRIMARY_STALE_SECS,
            OracleSource::Pyth      => PRIMARY_STALE_SECS,
            OracleSource::Twap      => TWAP_STALE_SECS,
        };
        self.age_secs < threshold
    }

    /// Relative price divergence from another price: |a âˆ’ b| / a.
    ///
    /// Returns `f64::INFINITY` when `self.price_usd` is zero.
    pub fn divergence_from(&self, other: &OraclePrice) -> f64 {
        if self.price_usd == 0.0 {
            return f64::INFINITY;
        }
        (self.price_usd - other.price_usd).abs() / self.price_usd
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OracleSource
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// The oracle backend that produced an `OraclePrice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleSource {
    /// Chainlink on-chain aggregator â€” primary.
    Chainlink,
    /// Pyth network off-chain aggregator â€” secondary.
    Pyth,
    /// Uniswap v3 time-weighted average price â€” tertiary fallback.
    Twap,
}

impl std::fmt::Display for OracleSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OracleSource::Chainlink => f.write_str("chainlink"),
            OracleSource::Pyth      => f.write_str("pyth"),
            OracleSource::Twap      => f.write_str("twap"),
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ResolvedPrice
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Output of tri-oracle resolution â€” the final price used by strategies.
#[derive(Debug, Clone)]
pub struct ResolvedPrice {
    /// The resolved USD price.
    pub price_usd: f64,
    /// Which source was selected.
    pub source: OracleSource,
    /// Age of the winning price in seconds.
    pub age_secs: u64,
    /// Block number of the winning price.
    pub block_number: u64,
    /// Whether both primary sources were fresh and agreed.
    pub dual_primary_agreement: bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// resolve_price
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Resolve the best available price from three oracle sources (spec Â§7).
///
/// ## Resolution order
///
///   1. Both primaries fresh + agree  â†’ Chainlink
///   2. Both primaries fresh + diverge â†’ `OmegaError::dropped(MissOracleDiverge)`
///   3. Chainlink fresh, Pyth stale   â†’ Chainlink
///   4. Pyth fresh, Chainlink stale   â†’ Pyth
///   5. Both stale, TWAP fresh        â†’ TWAP
///   6. All stale                     â†’ `OmegaError::dropped(MissOracle)`
pub fn resolve_price(
    chainlink: &OraclePrice,
    pyth:      &OraclePrice,
    twap:      &OraclePrice,
) -> Result<ResolvedPrice, OmegaError> {
    let cl_ok = chainlink.is_fresh();
    let py_ok = pyth.is_fresh();
    let tw_ok = twap.is_fresh();

    match (cl_ok, py_ok) {
        // â”€â”€ Both primaries fresh â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        (true, true) => {
            let div = chainlink.divergence_from(pyth);
            if div < DIVERGENCE_THRESHOLD {
                // Prices agree â€” Chainlink is authoritative
                tracing::debug!(
                    cl_price  = chainlink.price_usd,
                    py_price  = pyth.price_usd,
                    divergence = div,
                    "Tri-oracle: CL+Pyth agree â†’ Chainlink",
                );
                Ok(ResolvedPrice {
                    price_usd:              chainlink.price_usd,
                    source:                 OracleSource::Chainlink,
                    age_secs:               chainlink.age_secs,
                    block_number:           chainlink.block_number,
                    dual_primary_agreement: true,
                })
            } else {
                // Prices diverge â€” do not trade
                tracing::warn!(
                    cl_price   = chainlink.price_usd,
                    py_price   = pyth.price_usd,
                    divergence = div,
                    threshold  = DIVERGENCE_THRESHOLD,
                    "Tri-oracle: CL+Pyth diverge â†’ MissOracleDiverge",
                );
                Err(OmegaError::dropped(DropCode::MissOracleDiverge))
            }
        }

        // â”€â”€ Chainlink fresh, Pyth stale â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        (true, false) => {
            tracing::debug!(
                cl_age = chainlink.age_secs,
                py_age = pyth.age_secs,
                "Tri-oracle: Chainlink only",
            );
            Ok(ResolvedPrice {
                price_usd:              chainlink.price_usd,
                source:                 OracleSource::Chainlink,
                age_secs:               chainlink.age_secs,
                block_number:           chainlink.block_number,
                dual_primary_agreement: false,
            })
        }

        // â”€â”€ Pyth fresh, Chainlink stale â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        (false, true) => {
            tracing::debug!(
                cl_age = chainlink.age_secs,
                py_age = pyth.age_secs,
                "Tri-oracle: Pyth only",
            );
            Ok(ResolvedPrice {
                price_usd:              pyth.price_usd,
                source:                 OracleSource::Pyth,
                age_secs:               pyth.age_secs,
                block_number:           pyth.block_number,
                dual_primary_agreement: false,
            })
        }

        // â”€â”€ Both primaries stale â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
        (false, false) => {
            if tw_ok {
                tracing::warn!(
                    cl_age = chainlink.age_secs,
                    py_age = pyth.age_secs,
                    tw_age = twap.age_secs,
                    "Tri-oracle: both primaries stale â†’ TWAP fallback",
                );
                Ok(ResolvedPrice {
                    price_usd:              twap.price_usd,
                    source:                 OracleSource::Twap,
                    age_secs:               twap.age_secs,
                    block_number:           twap.block_number,
                    dual_primary_agreement: false,
                })
            } else {
                tracing::error!(
                    cl_age = chainlink.age_secs,
                    py_age = pyth.age_secs,
                    tw_age = twap.age_secs,
                    "Tri-oracle: all sources stale â†’ MissOracle",
                );
                Err(OmegaError::dropped(DropCode::MissOracle))
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

    fn price(usd: f64, source: OracleSource, age: u64) -> OraclePrice {
        OraclePrice {
            price_usd:    usd,
            source,
            age_secs:     age,
            block_number: 1_000_000,
            is_fallback:  matches!(source, OracleSource::Pyth | OracleSource::Twap),
        }
    }

    fn cl(usd: f64, age: u64)  -> OraclePrice { price(usd, OracleSource::Chainlink, age) }
    fn py(usd: f64, age: u64)  -> OraclePrice { price(usd, OracleSource::Pyth,      age) }
    fn tw(usd: f64, age: u64)  -> OraclePrice { price(usd, OracleSource::Twap,      age) }

    // â”€â”€ Rule 1: both fresh and agree â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn both_fresh_agree_returns_chainlink() {
        let r = resolve_price(&cl(1800.0, 10), &py(1801.0, 12), &tw(1790.0, 60)).unwrap();
        assert!(matches!(r.source, OracleSource::Chainlink));
        assert!((r.price_usd - 1800.0).abs() < 1e-6);
        assert!(r.dual_primary_agreement);
    }

    #[test]
    fn zero_divergence_agrees() {
        let r = resolve_price(&cl(1000.0, 0), &py(1000.0, 0), &tw(990.0, 30)).unwrap();
        assert!(matches!(r.source, OracleSource::Chainlink));
    }

    // â”€â”€ Rule 2: both fresh but diverge â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn both_fresh_diverge_returns_miss_oracle_diverge() {
        // 1800 vs 1808 â†’ divergence â‰ˆ 0.44% > 0.4% threshold
        let err = resolve_price(&cl(1800.0, 10), &py(1808.0, 12), &tw(1800.0, 60)).unwrap_err();
        assert_eq!(err.drop_code(), Some(DropCode::MissOracleDiverge));
    }

    #[test]
    fn exact_threshold_agrees() {
        // 1000 vs 1003.99 â†’ divergence = 0.399% < 0.4% â†’ should agree
        let r = resolve_price(&cl(1000.0, 1), &py(1003.99, 1), &tw(990.0, 30)).unwrap();
        assert!(matches!(r.source, OracleSource::Chainlink));
    }

    #[test]
    fn at_threshold_diverges() {
        // 1000 vs 1004.1 â†’ divergence = 0.41% â‰¥ 0.4% â†’ diverge
        let err = resolve_price(&cl(1000.0, 1), &py(1004.1, 1), &tw(990.0, 30)).unwrap_err();
        assert_eq!(err.drop_code(), Some(DropCode::MissOracleDiverge));
    }

    // â”€â”€ Rule 3: Chainlink only â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn chainlink_fresh_pyth_stale_returns_chainlink() {
        let r = resolve_price(&cl(1800.0, 10), &py(1800.0, 50), &tw(1800.0, 130)).unwrap();
        assert!(matches!(r.source, OracleSource::Chainlink));
        assert!(!r.dual_primary_agreement);
    }

    // â”€â”€ Rule 4: Pyth only â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn pyth_fresh_chainlink_stale_returns_pyth() {
        let r = resolve_price(&cl(1800.0, 50), &py(1799.0, 10), &tw(1800.0, 130)).unwrap();
        assert!(matches!(r.source, OracleSource::Pyth));
        assert!((r.price_usd - 1799.0).abs() < 1e-6);
    }

    // â”€â”€ Rule 5: TWAP fallback â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn both_primaries_stale_twap_fresh_returns_twap() {
        let r = resolve_price(&cl(1800.0, 50), &py(1799.0, 50), &tw(1795.0, 90)).unwrap();
        assert!(matches!(r.source, OracleSource::Twap));
        assert!((r.price_usd - 1795.0).abs() < 1e-6);
    }

    // â”€â”€ Rule 6: all stale â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn all_stale_returns_miss_oracle() {
        let err = resolve_price(&cl(1800.0, 50), &py(1799.0, 50), &tw(1795.0, 125)).unwrap_err();
        assert_eq!(err.drop_code(), Some(DropCode::MissOracle));
    }

    // â”€â”€ Staleness boundary â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn primary_at_44_seconds_is_fresh() {
        let r = resolve_price(&cl(1000.0, 44), &py(1000.0, 44), &tw(990.0, 30)).unwrap();
        assert!(r.dual_primary_agreement);
    }

    #[test]
    fn primary_at_45_seconds_is_stale() {
        // Both at 45s â€” both stale, TWAP fresh
        let r = resolve_price(&cl(1000.0, 45), &py(1000.0, 45), &tw(990.0, 90)).unwrap();
        assert!(matches!(r.source, OracleSource::Twap));
    }

    #[test]
    fn twap_at_119_seconds_is_fresh() {
        let r = resolve_price(&cl(1000.0, 50), &py(1000.0, 50), &tw(990.0, 119)).unwrap();
        assert!(matches!(r.source, OracleSource::Twap));
    }

    #[test]
    fn twap_at_120_seconds_is_stale() {
        let err = resolve_price(&cl(1000.0, 50), &py(1000.0, 50), &tw(990.0, 120)).unwrap_err();
        assert_eq!(err.drop_code(), Some(DropCode::MissOracle));
    }
}