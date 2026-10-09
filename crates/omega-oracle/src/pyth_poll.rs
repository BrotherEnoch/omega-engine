// omega-engine\crates\omega-oracle\src\pyth_poll.rs
//! Pyth Hermes HTTP polling loop — ingestion path for `PythOracle`.
//!
//! Since 2026-08-26 Hermes requires `Authorization: Bearer <PYTH_API_KEY>`.
//!
//! Env:
//! - `OMEGA_PYTH_HERMES_URL` — base (default https://hermes.pyth.network)
//! - `OMEGA_PYTH_API_KEY` / `PYTH_API_KEY` — required for live Hermes
//! - `OMEGA_PYTH_LOCAL_SEED=1` — seed static USD prices when no API key
//!   (local Anvil fork only)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::pyth::{arbitrum_price_ids, PythOracle};

pub const DEFAULT_HERMES_BASE: &str = "https://hermes.pyth.network";

fn hermes_base_from_env() -> String {
    std::env::var("OMEGA_PYTH_HERMES_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HERMES_BASE.to_owned())
}

fn api_key_from_env() -> Option<String> {
    std::env::var("OMEGA_PYTH_API_KEY")
        .or_else(|_| std::env::var("PYTH_API_KEY"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
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

#[derive(Debug, Deserialize)]
struct HermesLatestResponse {
    #[serde(default)]
    parsed: Vec<HermesParsedPrice>,
}

#[derive(Debug, Deserialize)]
struct HermesParsedPrice {
    id: String,
    price: HermesPriceFields,
}

#[derive(Debug, Deserialize)]
struct HermesPriceFields {
    price: String,
    conf: String,
    expo: i32,
    publish_time: i64,
}

fn scale_i64(raw: &str, expo: i32) -> Option<f64> {
    let v: i64 = raw.parse().ok()?;
    Some((v as f64) * 10f64.powi(expo))
}

fn symbol_by_price_id() -> HashMap<String, String> {
    let mut m = HashMap::new();
    for &(symbol, id) in arbitrum_price_ids() {
        let lower = id.to_lowercase();
        m.insert(lower.clone(), symbol.to_owned());
        m.insert(format!("0x{lower}"), symbol.to_owned());
    }
    m
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
            let conf = price * 0.001;
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
    let key = match api_key_from_env() {
        Some(k) => k,
        None => {
            if local_seed_enabled() {
                let n = seed_local_prices(pyth, block_number);
                tracing::debug!(updated = n, "Pyth local seed (no API key)");
                return Ok(n);
            }
            anyhow::bail!(
                "no OMEGA_PYTH_API_KEY/PYTH_API_KEY; set key or OMEGA_PYTH_LOCAL_SEED=1 for local fork"
            );
        }
    };

    let base = hermes_base_from_env();
    let ids: Vec<String> = arbitrum_price_ids()
        .iter()
        .map(|(_, id)| {
            if id.starts_with("0x") {
                id.to_string()
            } else {
                format!("0x{id}")
            }
        })
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }

    let mut url = format!("{base}/v2/updates/price/latest?");
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            url.push('&');
        }
        url.push_str("ids[]=");
        url.push_str(id);
    }

    let resp = http
        .get(&url)
        .header("Authorization", format!("Bearer {key}"))
        .timeout(Duration::from_secs(8))
        .send()
        .await?
        .error_for_status()?;

    let body: HermesLatestResponse = resp.json().await?;
    let map = symbol_by_price_id();
    let mut updated = 0usize;

    for item in body.parsed {
        let id_key = item.id.to_lowercase();
        let symbol = match map
            .get(&id_key)
            .or_else(|| map.get(&format!("0x{id_key}")))
        {
            Some(s) => s.as_str(),
            None => continue,
        };

        let price_usd = match scale_i64(&item.price.price, item.price.expo) {
            Some(p) if p.is_finite() && p > 0.0 => p,
            _ => {
                tracing::warn!(
                    symbol,
                    raw = %item.price.price,
                    expo = item.price.expo,
                    "Pyth Hermes: bad price scale — skip"
                );
                continue;
            }
        };
        let conf_usd = scale_i64(&item.price.conf, item.price.expo).unwrap_or(0.0);
        let publish_time = if item.price.publish_time > 0 {
            item.price.publish_time as u64
        } else {
            tracing::warn!(symbol, "Pyth Hermes: missing publish_time — skip");
            continue;
        };

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

    let has_key = api_key_from_env().is_some();
    let local = local_seed_enabled();
    tracing::info!(
        hermes = %hermes_base_from_env(),
        interval_secs = interval.as_secs(),
        feeds = arbitrum_price_ids().len(),
        has_api_key = has_key,
        local_seed = local,
        "L2p Pyth Hermes poll loop started"
    );

    if !has_key && local {
        let n = seed_local_prices(&pyth, 0);
        tracing::info!(updated = n, "Pyth local seed applied at startup");
    }

    loop {
        ticker.tick().await;
        match poll_hermes_once(&http, &pyth, 0).await {
            Ok(n) if n > 0 => {
                tracing::debug!(updated = n, "Pyth Hermes poll: cache refreshed");
            }
            Ok(_) => {
                tracing::warn!("Pyth Hermes poll: zero feeds updated");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Pyth Hermes poll failed — keeping previous cache");
                if local_seed_enabled() {
                    let n = seed_local_prices(&pyth, 0);
                    if n > 0 {
                        tracing::info!(updated = n, "Pyth local seed after Hermes failure");
                    }
                }
            }
        }
    }
}