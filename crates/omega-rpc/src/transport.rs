// crates/omega-rpc/src/transport.rs
//
// Transport selection, polling configuration, and provider construction.
//
// ## Why this module exists
//
//   omega-rpc originally spoke WebSocket only (eth_subscribe). A paid WSS key is
//   not always available, but plain HTTPS endpoints are free. The transport is
//   chosen from the URL SCHEME ALONE:
//
//     ws:// | wss://      → RpcTransport::Ws    (eth_subscribe streams)
//     http:// | https://  → RpcTransport::Http  (polling; see poll.rs)
//
//   There is deliberately no separate "force transport" override: a setting that
//   contradicts the URL it sits next to can only ever be a misconfiguration.
//
// ## What HTTP mode cannot do
//
//   Free HTTP endpoints do not expose a reliable mempool, so the pending-tx
//   stream is not available over HTTP (see subscriptions::run_pending_tx_stream).
//
// ## Polling configuration (read from the environment)
//
//   OMEGA_POLL_BLOCKS_MS       block poll interval, default 250, floor 50
//   OMEGA_POLL_LOGS_MS         log poll interval, default 1000, floor 200
//   OMEGA_LOGS_MAX_RANGE       max blocks per eth_getLogs call, default 1000, floor 1
//   OMEGA_POLL_MAX_CATCHUP     max blocks emitted after a stall, default 50, floor 1
//   OMEGA_LOGS_CONFIRMATIONS   logs are read up to (head - N); default 0. Raising it
//                              trades latency for protection against reorged logs,
//                              which HTTP log polling cannot otherwise replay.

use std::sync::Arc;
use std::time::Duration;

use alloy::providers::{Provider, ProviderBuilder, WsConnect};

use crate::net::{redact_ws_url, scrub_urls, validate_rpc_url, verify_chain_id, RpcClientError};

// ─────────────────────────────────────────────────────────────────────────────
// RpcTransport
// ─────────────────────────────────────────────────────────────────────────────

/// Which wire transport an RPC URL implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcTransport {
    /// WebSocket with eth_subscribe streams.
    Ws,
    /// Plain HTTP(S) JSON-RPC with polling.
    Http,
}

impl RpcTransport {
    /// Detects the transport from the URL scheme (case-insensitive). Anything that
    /// is not http:// or https:// is treated as WebSocket; structural validation is
    /// `net::validate_rpc_url`'s job, not this function's.
    pub fn detect(url: &str) -> Self {
        let u = url.trim().to_ascii_lowercase();
        if u.starts_with("http://") || u.starts_with("https://") {
            RpcTransport::Http
        } else {
            RpcTransport::Ws
        }
    }

    /// Short label for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RpcTransport::Ws => "websocket",
            RpcTransport::Http => "http-polling",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PollConfig
// ─────────────────────────────────────────────────────────────────────────────

const DEFAULT_BLOCKS_MS: u64 = 250;
const MIN_BLOCKS_MS: u64 = 50;
const DEFAULT_LOGS_MS: u64 = 1_000;
const MIN_LOGS_MS: u64 = 200;
const DEFAULT_LOGS_MAX_RANGE: u64 = 1_000;
const DEFAULT_MAX_CATCHUP: u64 = 50;
const DEFAULT_LOGS_CONFIRMATIONS: u64 = 0;

/// Polling cadence and bounds used by the HTTP transport path.
#[derive(Debug, Clone)]
pub struct PollConfig {
    /// How often the block / fee pollers ask for the chain tip.
    pub blocks_every: Duration,
    /// How often the log pollers query eth_getLogs.
    pub logs_every: Duration,
    /// Maximum block span of a single eth_getLogs call (split further on provider errors).
    pub logs_max_range: u64,
    /// Maximum number of blocks emitted after a stall; older ones are skipped (with a warning).
    pub max_catchup_blocks: u64,
    /// Logs are read only up to `head - logs_confirmations`.
    pub logs_confirmations: u64,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self::from_lookup(|_| None)
    }
}

impl PollConfig {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Environment-free constructor (testable). Unparseable values fall back to the
    /// default; values below a field's floor are clamped up to the floor.
    pub(crate) fn from_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let num = |key: &str, default: u64, min: u64| -> u64 {
            get(key)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|v| v.max(min))
                .unwrap_or(default)
        };
        Self {
            blocks_every: Duration::from_millis(num(
                "OMEGA_POLL_BLOCKS_MS",
                DEFAULT_BLOCKS_MS,
                MIN_BLOCKS_MS,
            )),
            logs_every: Duration::from_millis(num(
                "OMEGA_POLL_LOGS_MS",
                DEFAULT_LOGS_MS,
                MIN_LOGS_MS,
            )),
            logs_max_range: num("OMEGA_LOGS_MAX_RANGE", DEFAULT_LOGS_MAX_RANGE, 1),
            max_catchup_blocks: num("OMEGA_POLL_MAX_CATCHUP", DEFAULT_MAX_CATCHUP, 1),
            logs_confirmations: num("OMEGA_LOGS_CONFIRMATIONS", DEFAULT_LOGS_CONFIRMATIONS, 0),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider construction
// ─────────────────────────────────────────────────────────────────────────────

/// Opens a provider over the given transport and erases the concrete transport type.
///
/// Both arms end in `.boxed()` (`RootProvider::boxed`), producing
/// `RootProvider<BoxTransport, Ethereum>` — the instantiation that implements plain
/// `Provider` (see client.rs history for the E0277 this avoids). Error text is passed
/// through `scrub_urls` because reqwest/tungstenite errors embed the full URL, which
/// commonly carries an API key.
pub(crate) async fn open_provider(
    url: &str,
    transport: RpcTransport,
) -> Result<Arc<dyn Provider>, RpcClientError> {
    match transport {
        RpcTransport::Ws => {
            let provider = ProviderBuilder::new()
                .on_ws(WsConnect::new(url))
                .await
                .map_err(|e| {
                    RpcClientError::ConnectFailed(format!(
                        "WS connect: {}",
                        scrub_urls(&e.to_string())
                    ))
                })?;
            Ok(Arc::new(provider.boxed()))
        }
        RpcTransport::Http => {
            let parsed =
                url.trim()
                    .parse::<reqwest::Url>()
                    .map_err(|e| RpcClientError::InvalidUrl {
                        url: redact_ws_url(url),
                        reason: format!("HTTP URL parse: {e}"),
                    })?;
            let provider = ProviderBuilder::new().on_http(parsed);
            Ok(Arc::new(provider.boxed()))
        }
    }
}

/// Validates the URL, connects, and verifies the endpoint's chain ID in one step.
/// Every stream in subscriptions.rs goes through this so chain-ID verification cannot
/// be skipped at one call site while present at another.
pub(crate) async fn connect_verified(
    url: &str,
    expected_chain_id: u64,
) -> Result<(Arc<dyn Provider>, RpcTransport), RpcClientError> {
    let transport = validate_rpc_url(url)?;
    let provider = open_provider(url, transport).await?;
    verify_chain_id(provider.as_ref(), expected_chain_id).await?;
    Ok((provider, transport))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn detect_http_and_https() {
        assert_eq!(
            RpcTransport::detect("https://arb1.arbitrum.io/rpc"),
            RpcTransport::Http
        );
        assert_eq!(
            RpcTransport::detect("http://127.0.0.1:8545"),
            RpcTransport::Http
        );
        assert_eq!(
            RpcTransport::detect("  HTTPS://Example.com "),
            RpcTransport::Http
        );
    }

    #[test]
    fn detect_ws_and_wss() {
        assert_eq!(
            RpcTransport::detect("wss://node.example.com/ws"),
            RpcTransport::Ws
        );
        assert_eq!(
            RpcTransport::detect("ws://127.0.0.1:8546"),
            RpcTransport::Ws
        );
    }

    #[test]
    fn poll_config_defaults() {
        let c = PollConfig::from_lookup(lookup(&[]));
        assert_eq!(c.blocks_every, Duration::from_millis(250));
        assert_eq!(c.logs_every, Duration::from_millis(1_000));
        assert_eq!(c.logs_max_range, 1_000);
        assert_eq!(c.max_catchup_blocks, 50);
        assert_eq!(c.logs_confirmations, 0);
    }

    #[test]
    fn poll_config_reads_overrides() {
        let c = PollConfig::from_lookup(lookup(&[
            ("OMEGA_POLL_BLOCKS_MS", "500"),
            ("OMEGA_POLL_LOGS_MS", "2000"),
            ("OMEGA_LOGS_MAX_RANGE", "250"),
            ("OMEGA_POLL_MAX_CATCHUP", "10"),
            ("OMEGA_LOGS_CONFIRMATIONS", "2"),
        ]));
        assert_eq!(c.blocks_every, Duration::from_millis(500));
        assert_eq!(c.logs_every, Duration::from_millis(2_000));
        assert_eq!(c.logs_max_range, 250);
        assert_eq!(c.max_catchup_blocks, 10);
        assert_eq!(c.logs_confirmations, 2);
    }

    #[test]
    fn poll_config_clamps_to_floors_and_ignores_garbage() {
        let c = PollConfig::from_lookup(lookup(&[
            ("OMEGA_POLL_BLOCKS_MS", "1"),
            ("OMEGA_POLL_LOGS_MS", "10"),
            ("OMEGA_LOGS_MAX_RANGE", "0"),
            ("OMEGA_POLL_MAX_CATCHUP", "abc"),
        ]));
        assert_eq!(c.blocks_every, Duration::from_millis(MIN_BLOCKS_MS));
        assert_eq!(c.logs_every, Duration::from_millis(MIN_LOGS_MS));
        assert_eq!(c.logs_max_range, 1);
        assert_eq!(c.max_catchup_blocks, DEFAULT_MAX_CATCHUP);
    }
}
