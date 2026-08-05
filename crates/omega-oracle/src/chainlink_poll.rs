// crates/omega-oracle/src/chainlink_poll.rs
//
// Chainlink AggregatorV3 polling loop — the ingestion path
// `ChainlinkOracle` has been missing since it was introduced (this
// session's standing-queue item 2a, Chainlink leg).
//
// ## Why this lives here, not in omega-rpc
//
// This file references `crate::ChainlinkOracle` and calls
// `OmegaRpcClient::fetch_chainlink_round`. The confirmed dependency
// direction (Cargo.toml, checked this session) is `omega-oracle ->
// omega-rpc`, one-way only — `omega-rpc` has no dependency back on
// `omega-oracle`. Putting this loop in `omega-rpc` instead would need
// exactly that reverse edge, creating a cycle. `omega-oracle` already
// has the one dependency this file needs (`omega-rpc`), so it's the
// only crate that can hold both halves — the eth_call itself
// (`fetch_chainlink_round`, `AggregatorV3Interface`) stays in
// `omega-rpc`; only the "fetch then update the cache" wiring is here.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::Address;
use omega_rpc::OmegaRpcClient;

use crate::chainlink::{arbitrum_feeds, ChainlinkOracle};

/// Parse `arbitrum_feeds()`'s static `(symbol, address)` table into
/// real `Address` values once at startup, rather than re-parsing on
/// every poll cycle.
///
/// Returns `Err` on the first malformed address rather than silently
/// skipping it — same reasoning as `twap.rs`'s
/// `all_pool_addresses_are_well_formed` regression test: a malformed
/// feed address should be a loud startup failure, not a feed that
/// quietly never gets polled.
pub fn parse_arbitrum_chainlink_feeds() -> anyhow::Result<Vec<(String, Address)>> {
    arbitrum_feeds()
        .iter()
        .map(|(sym, addr)| {
            let a = Address::from_str(addr)
                .map_err(|e| anyhow::anyhow!("bad chainlink feed addr for {sym}: {e}"))?;
            Ok(((*sym).to_owned(), a))
        })
        .collect()
}

/// Poll Chainlink feeds on a fixed interval, refreshing only tokens
/// `ChainlinkOracle` currently considers stale.
///
/// Interval recommendation: 15–20s. `PRIMARY_STALE_SECS` (45s, per
/// `resolution.rs`) is the actual freshness bound the resolver enforces
/// — polling much faster than that burns read-rate budget for no
/// benefit, and `chainlink.rs`'s own doc comment already frames a
/// "slow refresh cycle" as the intended production shape rather than a
/// per-block read. The `is_stale` check before each fetch means a feed
/// that's already fresh (e.g. from a hypothetical future push-based
/// update path) isn't redundantly re-fetched every tick.
///
/// GAP, not fixed here: no caller in this codebase constructs a real
/// `HaltFlag`-integrated shutdown path for this specific loop — it runs
/// until its spawning task is cancelled/aborted, the same lifecycle as
/// the existing `run_canary_loop`/`run_scoring_loop` background tasks in
/// `main.rs`, which also have no individual halt check beyond
/// `halt.is_halted()`. Add one here too if this loop needs to stop
/// independently of process shutdown.
pub async fn run_chainlink_poll_loop(
    client: OmegaRpcClient,
    oracle: Arc<ChainlinkOracle>,
    feeds: Vec<(String, Address)>,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(
        feed_count = feeds.len(),
        interval_s = interval.as_secs(),
        "Chainlink poll loop starting",
    );

    loop {
        ticker.tick().await;

        for (symbol, feed) in &feeds {
            if !oracle.is_stale(symbol) {
                continue;
            }
            match client.fetch_chainlink_round(*feed, symbol).await {
                Ok(round) => {
                    oracle.update(
                        &round.token,
                        round.price_usd,
                        round.updated_at,
                        round.block_number,
                    );
                }
                Err(e) => {
                    tracing::warn!(token = %symbol, error = %e, "chainlink poll failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_feeds_produces_valid_addresses_or_fails_loudly() {
        // This test's outcome depends on arbitrum_feeds()'s real entries,
        // which this file hasn't independently verified beyond what
        // chainlink.rs's own doc comment claims. If this fails, that's
        // real signal about the feed table, not a bug in this parser —
        // same category as twap.rs's malformed-LINK-address regression
        // test.
        match parse_arbitrum_chainlink_feeds() {
            Ok(feeds) => assert!(!feeds.is_empty(), "expected at least one feed entry"),
            Err(e) => panic!(
                "arbitrum_feeds() contains a malformed address — fix the table, \
                 not this parser: {e}"
            ),
        }
    }
}
