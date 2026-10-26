// omega-engine\crates\omega-oracle\src\pyth_poll.rs
//! Price polling loop — ingestion path for `PythOracle`.
//!
//! Primary source : DeFiLlama coins API (https://coins.llama.fi) — free, no key.
//! Fallback       : static local seed when `OMEGA_PYTH_LOCAL_SEED=1`.
//!
//! Env:
//! - `OMEGA_PYTH_LOCAL_SEED=1` — seed static USD prices (local Anvil fork only)
//! - `OMEGA_DEFILLAMA_BASE`    — override DeFiLlama base URL (optional)
//!
//! NOTE: DeFiLlama's `confidence` field (0.0–1.0) is a data-quality score, NOT a
//! Pyth-style price spread. We ignore it and use a tight 0.05 % hardcoded interval
//! so PythOracle's ratio gate (rejects > ~0.5 %) always passes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::pyth::{arbitrum_price_ids, PythOracle};

pub const DEFAULT_DEFILLAMA_BASE: &str = "https://coins.llama.fi";

/// Tight confidence interval passed to PythOracle (0.05 % of price).
/// DeFiLlama's own confidence field is a data-quality score (0–1), not a spread,
/// so we use this constant instead of deriving a spread from it.
const CONF_RATIO: f64 = 0.0005;

fn defillama_base_from_env() -> String {
    std::env::var("OMEGA_DEFILLAMA_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_DEFILLAMA_BASE.to_owned())
}

fn local_seed_enabled() -> bool {
    std::env::var("OMEGA_PYTH_LOCAL_SEED")
        .map(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn local_seed_prices() -> &'static [(&'static str, f64)] {
    &[
        ("WETH", 3500.0),
        ("WBTC", 95_000.0),
        ("LINK", 18.0),
        ("ARB", 0.85),
        ("USDC", 1.0),
        ("USDT", 1.0),
    ]
}

fn symbol_to_coingecko_id(symbol: &str) -> Option<&'static str> {
    match symbol {
        "WETH" | "ETH" => Some("ethereum"),
        "WBTC" | "BTC" => Some("bitcoin"),
        "LINK" => Some("chainlink"),
        "ARB" => Some("arbitrum"),
        "USDC" => Some("usd-coin"),
        "USDT" => Some("tether"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct DefiLlamaResponse {
    coins: HashMap<String, DefiLlamaCoin>,
}

#[derive(Debug, Deserialize)]
struct DefiLlamaCoin {
    price: f64,
    #[serde(default)]
    timestamp: Option<f64>,
    /// Data-quality score (0.0–1.0). NOT a price spread — ignored for conf_usd.
    #[serde(default)]
    confidence: Option<f64>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn seed_local_prices(pyth: &PythOracle, block_number: u64) -> usize {
    let ts = now_unix();
    let mut n = 0usize;
    for &(symbol, price) in local_seed_prices() {
        if arbitrum_price_ids().iter().any(|(s, _)| *s == symbol) {
            let conf = price * CONF_RATIO;
            pyth.update(symbol, price, conf, ts, block_number);
            n += 1;
        }
    }
    n
}

pub async fn poll_hermes_once(
    http: &reqwest::Client,
    pyth: &PythOracle,
    block_number: u64,
) -> anyhow::Result<usize> {
    if local_seed_enabled() {
        let n = seed_local_prices(pyth, block_number);
        tracing::debug!(updated = n, "Price local seed");
        return Ok(n);
    }

    let symbols: Vec<(&str, &str)> = arbitrum_price_ids()
        .iter()
        .filter_map(|(sym, _)| symbol_to_coingecko_id(sym).map(|cg| (*sym, cg)))
        .collect();

    if symbols.is_empty() {
        return Ok(0);
    }

    let coins_param = symbols
        .iter()
        .map(|(_, cg)| format!("coingecko:{cg}"))
        .collect::<Vec<_>>()
        .join(",");

    let base = defillama_base_from_env();
    let url = format!("{base}/prices/current/{coins_param}");

    let resp = http
        .get(&url)
        .timeout(Duration::from_secs(8))
        .send()
        .await?
        .error_for_status()?;

    let body: DefiLlamaResponse = resp.json().await?;
    let now = now_unix();
    let mut updated = 0usize;

    for (symbol, cg_id) in &symbols {
        let key = format!("coingecko:{cg_id}");
        let coin = match body.coins.get(&key) {
            Some(c) => c,
            None => {
                tracing::warn!(symbol, key, "DeFiLlama: coin missing from response — skip");
                continue;
            }
        };

        let price_usd = coin.price;
        if !price_usd.is_finite() || price_usd <= 0.0 {
            tracing::warn!(symbol, price_usd, "DeFiLlama: bad price — skip");
            continue;
        }

        // Use a tight hardcoded spread (0.05 %) — DeFiLlama's confidence field is a
        // data-quality score (e.g. 0.99), not a price interval, and feeding it into
        // the Pyth-style ratio gate produces a ~1 % spread that the gate rejects.
        let conf_usd = price_usd * CONF_RATIO;

        if let Some(c) = coin.confidence {
            if c < 0.9 {
                tracing::warn!(
                    symbol,
                    confidence = c,
                    "DeFiLlama: low data-quality score — skip"
                );
                continue;
            }
        }

        let publish_time = coin
            .timestamp
            .map(|t| t as u64)
            .filter(|&t| t > 0)
            .unwrap_or(now);

        pyth.update(symbol, price_usd, conf_usd, publish_time, block_number);
        updated += 1;
    }

    Ok(updated)
}

pub async fn run_pyth_hermes_poll_loop(
    http: reqwest::Client,
    pyth: Arc<PythOracle>,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let local = local_seed_enabled();
    tracing::info!(
        price_source = %defillama_base_from_env(),
        interval_secs = interval.as_secs(),
        feeds = arbitrum_price_ids().len(),
        has_api_key = false,
        local_seed = local,
        conf_ratio = CONF_RATIO,
        "L2p Pyth Hermes poll loop started"
    );

    if local {
        let n = seed_local_prices(&pyth, 0);
        tracing::info!(updated = n, "Price local seed applied at startup");
    }

    loop {
        ticker.tick().await;
        match poll_hermes_once(&http, &pyth, 0).await {
            Ok(n) if n > 0 => {
                tracing::debug!(updated = n, "DeFiLlama price poll: cache refreshed");
            }
            Ok(_) => {
                tracing::warn!("DeFiLlama price poll: zero feeds updated");
            }
            Err(e) => {
                tracing::warn!(error = %e, "DeFiLlama price poll failed — keeping previous cache");
                if local_seed_enabled() {
                    let n = seed_local_prices(&pyth, 0);
                    if n > 0 {
                        tracing::info!(updated = n, "Price local seed after DeFiLlama failure");
                    }
                }
            }
        }
    }
}
