// crates/omega-oracle/src/pyth_poll.rs
//
// Pyth ingestion path for the secondary oracle cache (`PythOracle`).
//
// ## Why this lives here (same reasoning as chainlink_poll.rs)
//
// References `crate::PythOracle`. Dependency direction is
// `omega-oracle -> omega-rpc` only. A live Hermes/SSE path would call
// into omega-rpc; until that RPC helper exists, this module provides:
//
//   1. Local seed mode for Anvil / fork tests (`OMEGA_PYTH_LOCAL_SEED=1`)
//   2. A poll-loop skeleton that only refreshes stale symbols
//
// Production Hermes polling is intentionally a no-op until a real
// `OmegaRpcClient::fetch_pyth_price` (or equivalent) exists — fail closed,
// never invent prices.
//
// ## Local seed (fork testing)
//
// When `OMEGA_PYTH_LOCAL_SEED` is set to a non-empty value other than "0",
// the loop seeds `PythOracle` with fixed USD prices for the Arbitrum
// symbol table. Those prices must be near the forked pool prices or the
// tri-oracle divergence check (check 8 / MissOracleDiverge) will drop
// SA/MSA. For a short diagnostic run you can raise the divergence
// threshold to ~1.0; for longer fork testing, set the seed prices near
// the pool instead and leave the threshold at ~0.25.

use std::env;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::pyth::{arbitrum_price_ids, PythOracle};

/// Default local-seed USD prices for Arbitrum symbols.
///
/// Order matches `arbitrum_price_ids()`. Override individual values via
/// env if your fork’s pool mid is far from these (e.g. after a large
/// move): `OMEGA_PYTH_SEED_WETH=3250.5` etc.
fn default_local_seed_prices() -> Vec<(String, f64)> {
    vec![
        ("WETH".into(), 3_200.0),
        ("WBTC".into(), 95_000.0),
        ("LINK".into(), 14.5),
        ("ARB".into(), 0.55),
        ("USDC".into(), 1.0),
        ("USDT".into(), 1.0),
    ]
}

fn env_seed_price(symbol: &str, fallback: f64) -> f64 {
    let key = format!("OMEGA_PYTH_SEED_{symbol}");
    env::var(&key)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p > 0.0)
        .unwrap_or(fallback)
}

fn local_seed_enabled() -> bool {
    match env::var("OMEGA_PYTH_LOCAL_SEED") {
        Ok(v) => {
            let t = v.trim();
            !t.is_empty() && t != "0" && !t.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One-shot local seed: write tight-confidence prices into `PythOracle`
/// for every Arbitrum symbol. Safe to call at startup before the loop.
///
/// Confidence is 0.1% of price so `MAX_CONFIDENCE_RATIO` (1%) accepts it.
pub fn seed_local_pyth_prices(oracle: &PythOracle, block_number: u64) {
    let defaults = default_local_seed_prices();
    let publish_time = now_secs();

    for (symbol, default_px) in defaults {
        let price_usd = env_seed_price(&symbol, default_px);
        let confidence_usd = (price_usd * 0.001).max(1e-6);
        oracle.update(
            &symbol,
            price_usd,
            confidence_usd,
            publish_time,
            block_number,
        );
        tracing::info!(
            token = %symbol,
            price_usd,
            confidence_usd,
            "Pyth local seed applied",
        );
    }
}

/// Symbols that `PythOracle` is expected to track on Arbitrum (from the
/// static price-id table). Used by the poll loop’s staleness sweep.
pub fn arbitrum_pyth_symbols() -> Vec<String> {
    arbitrum_price_ids()
        .iter()
        .map(|(sym, _)| (*sym).to_owned())
        .collect()
}

/// Poll / refresh loop for the Pyth secondary cache.
///
/// Behaviour:
/// - If `OMEGA_PYTH_LOCAL_SEED` is enabled: re-seed stale symbols each tick
///   so the cache never ages out during a long Anvil session.
/// - Otherwise: log once that live Hermes ingestion is not wired and idle
///   (fail closed — do not fabricate prices).
///
/// Interval recommendation: 15–20s (same as Chainlink). `PRIMARY_STALE_SECS`
/// is 45s; polling much faster only burns work.
pub async fn run_pyth_poll_loop(oracle: Arc<PythOracle>, interval: Duration) {
    let symbols = arbitrum_pyth_symbols();
    let local = local_seed_enabled();

    tracing::info!(
        symbol_count = symbols.len(),
        interval_s = interval.as_secs(),
        local_seed = local,
        "Pyth poll loop starting",
    );

    if local {
        // Initial seed so the first resolution pass is not empty.
        seed_local_pyth_prices(&oracle, 0);
    } else {
        tracing::warn!(
            "Pyth live ingestion is not wired (no Hermes/RPC helper yet). \
             Cache stays empty unless OMEGA_PYTH_LOCAL_SEED=1. Resolution \
             fails closed when Chainlink+TWAP are also stale."
        );
    }

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;

        if !local {
            // Nothing to do until a real fetch path exists.
            continue;
        }

        // Re-seed only stale symbols so publish_time stays fresh under
        // PRIMARY_STALE_SECS during long fork runs.
        let defaults = default_local_seed_prices();
        let publish_time = now_secs();
        for (symbol, default_px) in &defaults {
            if !oracle.is_stale(symbol) {
                continue;
            }
            let price_usd = env_seed_price(symbol, *default_px);
            let confidence_usd = (price_usd * 0.001).max(1e-6);
            oracle.update(symbol, price_usd, confidence_usd, publish_time, 0);
            tracing::debug!(
                token = %symbol,
                price_usd,
                "Pyth local seed refreshed (was stale)",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrum_symbols_non_empty() {
        let s = arbitrum_pyth_symbols();
        assert!(!s.is_empty());
        assert!(s.iter().any(|x| x == "WETH"));
    }

    #[test]
    fn seed_writes_readable_weth() {
        let o = PythOracle::new(42161);
        seed_local_pyth_prices(&o, 1);
        let p = o.read("WETH").expect("WETH should be readable after seed");
        assert!(p.price_usd > 0.0);
        assert!(!o.is_stale("WETH"));
    }

    #[test]
    fn default_seed_table_covers_price_id_table() {
        let ids: Vec<_> = arbitrum_price_ids().iter().map(|(s, _)| *s).collect();
        let seeds: Vec<_> = default_local_seed_prices()
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        for id in ids {
            assert!(
                seeds.iter().any(|s| s == id),
                "missing default seed for {id}"
            );
        }
    }
}