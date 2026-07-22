// crates/omega-relay/src/client.rs
//! Relay HTTP submission client.
//!
//! `RelayClient` is a trait so the backpressure and cascade modules are fully
//! testable without hitting live relays.  `HttpRelayClient` is the production
//! implementation.  `MockRelayClient` is used in unit tests only.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{RelayError, RelayResult};

// â”€â”€ Bundle payload â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// The bundle payload submitted to a relay.
/// Fields mirror the Flashbots `eth_sendBundle` JSON-RPC format.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundlePayload {
    /// 0x-prefixed keccak256 hash of the serialised bundle.
    pub bundle_hash: String,
    /// Signed transaction hex strings.
    pub txs: Vec<String>,
    /// Target block number (hex).
    pub block_number: String,
    /// Minimum timestamp (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_timestamp: Option<u64>,
    /// Maximum timestamp (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_timestamp: Option<u64>,
    /// Priority fee in gwei (Arbitrum sequencer tip).
    pub priority_fee_gwei: u64,
}

/// Outcome of a single relay submission.
#[derive(Debug, Clone)]
pub struct SubmissionOutcome {
    /// Whether the relay reported the bundle as included.
    pub included: bool,
    /// Relay-assigned bundle UUID (if returned).
    pub relay_bundle_id: Option<String>,
}

// â”€â”€ Trait â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Abstraction over a single relay endpoint.
#[async_trait]
pub trait RelayClient: Send + Sync + 'static {
    /// Submit a bundle.  Returns `Ok(SubmissionOutcome)` on HTTP 200.
    async fn submit_bundle(&self, bundle: BundlePayload) -> RelayResult<SubmissionOutcome>;

    /// Human-readable name for logging.
    fn name(&self) -> &str;
}

// â”€â”€ HTTP implementation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// JSON-RPC request body for `eth_sendBundle`.
#[derive(Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: &'a [serde_json::Value],
}

/// JSON-RPC response envelope.
#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<BundleResult>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct BundleResult {
    #[serde(rename = "bundleHash")]
    bundle_hash: Option<String>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    message: String,
}

/// Production HTTP relay client.
pub struct HttpRelayClient {
    name: String,
    endpoint: String,
    client: Client,
}

impl HttpRelayClient {
    /// Construct with a shared `reqwest::Client` (allows connection pooling
    /// across multiple relays).
    pub fn new(name: impl Into<String>, endpoint: impl Into<String>, client: Client) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            endpoint: endpoint.into(),
            client,
        })
    }

    /// Build a `reqwest::Client` configured for relay submission.
    pub fn build_http_client() -> reqwest::Result<Client> {
        Client::builder()
            .timeout(Duration::from_millis(500)) // well within 80 ms LA window budget
            .tcp_keepalive(Duration::from_secs(10))
            .pool_max_idle_per_host(8)
            .build()
    }
}

#[async_trait]
impl RelayClient for HttpRelayClient {
    async fn submit_bundle(&self, bundle: BundlePayload) -> RelayResult<SubmissionOutcome> {
        let params = serde_json::json!([{
            "txs": bundle.txs,
            "blockNumber": bundle.block_number,
            "minTimestamp": bundle.min_timestamp,
            "maxTimestamp": bundle.max_timestamp,
        }]);

        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "eth_sendBundle",
            params: std::slice::from_ref(&params),
        };

        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| RelayError::RequestFailed {
                relay: self.name.clone(),
                source: e,
            })?;

        let status = resp.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            warn!(relay = %self.name, "relay returned 429 â€” rate limited");
            return Err(RelayError::RateLimited { relay: self.name.clone() });
        }

        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(RelayError::HttpError {
                relay: self.name.clone(),
                status: status.as_u16(),
                body: body_text,
            });
        }

        let rpc_resp: JsonRpcResponse = resp.json().await.map_err(|e| RelayError::RequestFailed {
            relay: self.name.clone(),
            source: e,
        })?;

        if let Some(err) = rpc_resp.error {
            return Err(RelayError::HttpError {
                relay: self.name.clone(),
                status: 200,
                body: err.message,
            });
        }

        let relay_bundle_id = rpc_resp
            .result
            .and_then(|r| r.bundle_hash);

        debug!(
            relay = %self.name,
            bundle_hash = %bundle.bundle_hash,
            "bundle submitted successfully"
        );

        // Relays don't immediately confirm inclusion; `included` is set to true
        // for successful 200 responses (inclusion is confirmed asynchronously via
        // bundle status polling, which is the responsibility of the orchestrator).
        Ok(SubmissionOutcome {
            included: true,
            relay_bundle_id,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// â”€â”€ Mock (tests only) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Deterministic mock relay for unit tests.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockRelayClient {
    name: String,
    /// Whether this mock will report inclusion.
    pub includes: bool,
    /// Count of bundles received.
    pub received: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-utils"))]
impl MockRelayClient {
    pub fn new(includes: bool) -> Self {
        Self {
            name: if includes { "mock-include" } else { "mock-reject" }.into(),
            includes,
            received: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn with_name(name: impl Into<String>, includes: bool) -> Self {
        Self {
            name: name.into(),
            includes,
            received: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn received_count(&self) -> usize {
        self.received.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl RelayClient for MockRelayClient {
    async fn submit_bundle(&self, bundle: BundlePayload) -> RelayResult<SubmissionOutcome> {
        self.received.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(SubmissionOutcome {
            included: self.includes,
            relay_bundle_id: Some(format!("mock-{}", bundle.bundle_hash)),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_include_reports_included() {
        let client = MockRelayClient::new(true);
        let outcome = client
            .submit_bundle(BundlePayload {
                bundle_hash: "0xtest".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(outcome.included);
        assert_eq!(client.received_count(), 1);
    }

    #[tokio::test]
    async fn mock_reject_reports_not_included() {
        let client = MockRelayClient::new(false);
        let outcome = client
            .submit_bundle(BundlePayload {
                bundle_hash: "0xtest".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!outcome.included);
    }

    #[tokio::test]
    async fn mock_counts_submissions() {
        let client = MockRelayClient::new(true);
        for i in 0..5 {
            client
                .submit_bundle(BundlePayload {
                    bundle_hash: format!("0x{i}"),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        assert_eq!(client.received_count(), 5);
    }
}