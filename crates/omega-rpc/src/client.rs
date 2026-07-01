// crates/omega-rpc/src/client.rs
//
// OmegaRpcClient — rate-limited WebSocket RPC client.
//
// ## Architectural role (§22.1)
//
//   omega-rpc is the ONLY crate permitted to use the full alloy transport
//   stack.  All other crates receive data through the oracle layer
//   (omega-oracle) or via the OracleSignal channel, never via direct
//   RPC calls.
//
// ## Connection model
//
//   Single WebSocket connection to a dedicated node (500 rps target).
//   Reconnection is handled by `connect_with_retry` — exponential
//   backoff from 1s to 30s.
//
// ## Transport selection
//
//   `ProviderBuilder::on_builtin` performs scheme-based transport
//   detection.  With `transport-ws` and `pubsub` features compiled in
//   (guaranteed by the workspace Cargo.toml), `on_builtin` correctly
//   routes `wss://` and `ws://` URLs to the WebSocket transport.
//   The previous error "invalid URL scheme: wss" was caused by
//   `default-features = false` at the workspace level suppressing those
//   features — that is fixed in Cargo.toml.
//
// ## Health integration
//
//   `OmegaRpcClient` holds an optional `Arc<dyn LayerHealth>` for the
//   ExternalData layer.  When the WS connection drops or the rate
//   limiter is saturated, the health layer is transitioned to Degraded.
//   When connectivity is restored, it is recovered to Healthy.

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::B256;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Block, Filter, Log};
use futures::StreamExt;
use tokio::sync::broadcast;

use omega_core::{FeeSnapshot, HealthState, LayerHealth};

use crate::rate_limiter::{RpcRateLimiter, RpcRequestKind};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const RECONNECT_DELAY_INITIAL_MS: u64 = 1_000;
const RECONNECT_DELAY_MAX_MS:     u64 = 30_000;
const BLOCK_CHANNEL_CAPACITY:  usize  = 64;

// ─────────────────────────────────────────────────────────────────────────────
// BlockEvent
// ─────────────────────────────────────────────────────────────────────────────

/// Lightweight block header event emitted on each new block.
///
/// Downstream consumers (reorg guard, fee oracle, LA tier monitor) receive
/// this via a `broadcast::Receiver<BlockEvent>`.
#[derive(Debug, Clone)]
pub struct BlockEvent {
    pub number:        u64,
    pub hash:          B256,
    /// EIP-1559 base fee in gwei.  `None` for pre-London blocks.
    pub base_fee_gwei: Option<u64>,
    /// Unix timestamp in seconds.
    pub timestamp:     u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// RpcClientConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Runtime configuration for `OmegaRpcClient`.
#[derive(Debug, Clone)]
pub struct RpcClientConfig {
    /// WebSocket endpoint URL (wss:// or ws://).
    pub ws_url:    String,
    /// Requests per second budget (controls rate limiter config).
    pub rps_limit: u32,
    /// EIP-155 chain ID — used to stamp outbound signals.
    pub chain_id:  u64,
}

impl RpcClientConfig {
    pub fn new(ws_url: impl Into<String>, rps_limit: u32, chain_id: u64) -> Self {
        Self { ws_url: ws_url.into(), rps_limit, chain_id }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaRpcClient
// ─────────────────────────────────────────────────────────────────────────────

/// Rate-limited WebSocket RPC client for the Omega Engine.
///
/// Wraps an alloy provider with:
///   - Token-bucket rate limiting per request kind (§22 hardware spec)
///   - Block header broadcast channel for downstream consumers
///   - Health layer integration for ExternalData transitions
///   - Automatic reconnect with exponential backoff
///
/// Cloning is cheap — all fields are `Arc`-wrapped.
#[derive(Clone)]
pub struct OmegaRpcClient {
    config:       RpcClientConfig,
    rate_limiter: RpcRateLimiter,
    block_tx:     broadcast::Sender<BlockEvent>,
    health:       Option<Arc<dyn LayerHealth>>,
}

impl OmegaRpcClient {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Connect to the given WebSocket endpoint.
    ///
    /// Returns an error if the initial connection fails.  Use
    /// `connect_with_retry` when the caller can tolerate initial failure.
    pub async fn connect(config: RpcClientConfig) -> anyhow::Result<Self> {
        ProviderBuilder::new()
            .on_builtin(&config.ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("WS connect failed: {e}"))?;

        let (block_tx, _) = broadcast::channel(BLOCK_CHANNEL_CAPACITY);

        let limiter = if config.rps_limit > 0 {
            let read_cap = (config.rps_limit as f64 * 0.80) as u32;
            let writ_cap = (config.rps_limit as f64 * 0.10) as u32;
            let sub_cap  = (config.rps_limit as f64 * 0.04) as u32;
            RpcRateLimiter::with_config(
                crate::rate_limiter::BucketConfig { capacity: read_cap, refill_per_second: read_cap },
                crate::rate_limiter::BucketConfig { capacity: writ_cap, refill_per_second: writ_cap },
                crate::rate_limiter::BucketConfig { capacity: sub_cap,  refill_per_second: sub_cap  },
            )
        } else {
            RpcRateLimiter::new()
        };

        tracing::info!(
            ws_url   = %config.ws_url,
            chain_id = config.chain_id,
            rps      = config.rps_limit,
            "OmegaRpcClient connected",
        );

        Ok(Self { config, rate_limiter: limiter, block_tx, health: None })
    }

    /// Connect with exponential backoff retry.
    ///
    /// Retries indefinitely until connection succeeds.
    /// Backoff: 1s → 2s → 4s → 8s → … → 30s cap.
    pub async fn connect_with_retry(config: RpcClientConfig) -> Self {
        let mut delay_ms = RECONNECT_DELAY_INITIAL_MS;
        loop {
            match Self::connect(config.clone()).await {
                Ok(client) => return client,
                Err(e) => {
                    tracing::warn!(
                        error    = %e,
                        delay_ms,
                        ws_url   = %config.ws_url,
                        "RPC connect failed — retrying",
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(RECONNECT_DELAY_MAX_MS);
                }
            }
        }
    }

    /// Wire in the ExternalData health layer.
    pub fn with_health(mut self, health: Arc<dyn LayerHealth>) -> Self {
        self.health = Some(health);
        self
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    /// Subscribe to new block headers.
    pub fn subscribe_blocks(&self) -> broadcast::Receiver<BlockEvent> {
        self.block_tx.subscribe()
    }

    /// Start the block header subscription background task.
    ///
    /// Must be spawned as a Tokio task.  Reconnects automatically on
    /// stream end.  Never returns under normal operation.
    pub async fn run_block_subscription(&self) {
        let mut delay_ms = RECONNECT_DELAY_INITIAL_MS;
        loop {
            match self.run_block_subscription_once().await {
                Ok(()) => {
                    tracing::warn!("Block header stream ended — reconnecting");
                    delay_ms = RECONNECT_DELAY_INITIAL_MS;
                }
                Err(e) => {
                    tracing::error!(error = %e, "Block header stream error — reconnecting");
                    if let Some(ref health) = self.health {
                        health.set_state(
                            HealthState::Degraded,
                            &format!("RPC block stream error: {e}"),
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(RECONNECT_DELAY_MAX_MS);
                }
            }
        }
    }

    async fn run_block_subscription_once(&self) -> anyhow::Result<()> {
        self.rate_limiter
            .wait_until_allowed(RpcRequestKind::Subscribe)
            .await;

        let provider = ProviderBuilder::new()
            .on_builtin(&self.config.ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("WS reconnect failed: {e}"))?;

        if let Some(ref health) = self.health {
            if health.state() == HealthState::Degraded {
                health.set_state(HealthState::Healthy, "RPC block stream reconnected");
            }
        }

        let mut stream = provider
            .subscribe_blocks()
            .await
            .map_err(|e| anyhow::anyhow!("subscribe_blocks failed: {e}"))?
            .into_stream();

        tracing::info!("Block header subscription active");

        while let Some(block) = stream.next().await {
            let event = block_to_event(&block);
            tracing::debug!(
                block_number = event.number,
                hash         = %event.hash,
                base_fee     = ?event.base_fee_gwei,
                "New block",
            );
            let _ = self.block_tx.send(event);
        }

        Ok(())
    }

    // ── Rate-limited calls ────────────────────────────────────────────────────

    /// Execute a read after consuming a rate-limit token.
    pub async fn gated_read<F, Fut, T>(
        &self,
        wait_timeout: Option<Duration>,
        f: F,
    ) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        match wait_timeout {
            Some(timeout) => {
                tokio::time::timeout(
                    timeout,
                    self.rate_limiter.wait_until_allowed(RpcRequestKind::Read),
                )
                .await
                .map_err(|_| anyhow::anyhow!("RPC read rate-limit timeout"))?;
            }
            None => {
                self.rate_limiter
                    .wait_until_allowed(RpcRequestKind::Read)
                    .await;
            }
        }
        f().await
    }

    /// Execute a write after consuming a rate-limit token.
    pub async fn gated_write<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        self.rate_limiter
            .wait_until_allowed(RpcRequestKind::Write)
            .await;
        f().await
    }

    // ── Fee oracle ────────────────────────────────────────────────────────────

    /// Fetch the current fee snapshot from the node.
    pub async fn fetch_fee_snapshot(&self) -> anyhow::Result<FeeSnapshot> {
        let ws_url = self.config.ws_url.clone();
        self.gated_read(None, || async move {
            let provider = ProviderBuilder::new()
                .on_builtin(&ws_url)
                .await
                .map_err(|e| anyhow::anyhow!("fee snapshot connect: {e}"))?;

            let block = provider
                .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest, false)
                .await
                .map_err(|e| anyhow::anyhow!("eth_getBlockByNumber failed: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("latest block not found"))?;

            let base_fee_gwei =
                block.header.base_fee_per_gas.unwrap_or(0) / 1_000_000_000;

            Ok(FeeSnapshot {
                base_fee_gwei:     base_fee_gwei as u64,
                l1_data_fee_gwei:  0,
                priority_fee_gwei: 0,
                block_number:      block.header.number,
            })
        })
        .await
    }

    /// Fetch logs matching a filter, rate-limited as a read.
    pub async fn fetch_logs(&self, filter: Filter) -> anyhow::Result<Vec<Log>> {
        let ws_url = self.config.ws_url.clone();
        self.gated_read(None, || async move {
            let provider = ProviderBuilder::new()
                .on_builtin(&ws_url)
                .await
                .map_err(|e| anyhow::anyhow!("log fetch connect: {e}"))?;

            provider
                .get_logs(&filter)
                .await
                .map_err(|e| anyhow::anyhow!("eth_getLogs failed: {e}"))
        })
        .await
    }

    // ── Telemetry ─────────────────────────────────────────────────────────────

    /// Rate limiter snapshot for the shadow scorecard `rpc_headroom` metric.
    pub async fn rate_limiter_snapshot(&self) -> crate::rate_limiter::RateLimiterSnapshot {
        self.rate_limiter.snapshot().await
    }

    /// Chain ID this client is connected to.
    pub fn chain_id(&self) -> u64 {
        self.config.chain_id
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn block_to_event(block: &Block) -> BlockEvent {
    let base_fee_gwei = block
        .header
        .base_fee_per_gas
        .map(|fee| (fee / 1_000_000_000) as u64);

    BlockEvent {
        number:        block.header.number,
        hash:          block.header.hash,
        base_fee_gwei,
        timestamp:     block.header.timestamp,
    }
}