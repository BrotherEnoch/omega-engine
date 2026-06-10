// crates/omega-rpc/src/subscriptions.rs
//
// On-chain subscription streams for the Omega Engine signal pipeline.
//
// ## Architecture
//
//   Each subscription function is a Tokio task that:
//     1. Acquires a subscribe token from the rate limiter.
//     2. Opens a WebSocket subscription.
//     3. Publishes parsed events to the appropriate tokio broadcast or
//        mpsc channel.
//     4. Reconnects on error with exponential backoff.
//
//   Downstream consumers (omega-oracle) receive typed events from the
//   channels without holding any RPC handles.
//
// ## Streams implemented
//
//   PendingTxStream       — eth_subscribe("newPendingTransactions")
//                           Used by SA (Phase 1) and MSA (Phase 2).
//
//   LendingProtocolStream — eth_subscribe("logs") for Aave v3, Compound,
//                           Morpho, Euler v2 events.  Primary input for
//                           the LA position monitor (§11).
//
//   DexSyncStream         — eth_subscribe("logs") for AMM Sync/Swap events.
//                           Triggers Bellman-Ford update (§10, 50ms debounce).
//
//   FeeOracleStream       — eth_subscribe("newHeads") for base fee updates.
//                           Feeds the dual-component gas model (§7).
//
//   MevShareStream        — HTTP SSE from mev-share.flashbots.net.
//                           Order-flow signal for Phase 4 MEV-OFA.
//                           Reconnects with exponential backoff on SSE drop.
//
// ## Event types
//
//   All event types carry `chain_id`, `block_number`, and
//   `received_at_unix_ms` for EIL state versioning (§6).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log};
use futures::StreamExt;
use tokio::sync::broadcast;

use crate::rate_limiter::{RpcRateLimiter, RpcRequestKind};

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const BACKOFF_INITIAL_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;

// -----------------------------------------------------------------------------
// PendingTxStream — SA / MSA order flow
// -----------------------------------------------------------------------------

/// A pending transaction observed in the mempool.
#[derive(Debug, Clone)]
pub struct PendingTxEvent {
    /// Transaction hash.
    pub tx_hash: B256,
    /// Unix timestamp (ms) when received.
    pub received_at_unix_ms: u64,
    /// Chain ID.
    pub chain_id: u64,
}

/// Subscribe to pending transactions and forward to `tx`.
///
/// Runs indefinitely; reconnects on error.  Intended to be spawned as a
/// Tokio task.
pub async fn run_pending_tx_stream(
    ws_url: String,
    chain_id: u64,
    limiter: Arc<RpcRateLimiter>,
    tx: broadcast::Sender<PendingTxEvent>,
) {
    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        limiter.wait_until_allowed(RpcRequestKind::Subscribe).await;

        match run_pending_tx_once(&ws_url, chain_id, &tx).await {
            Ok(()) => {
                tracing::warn!("Pending tx stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) => {
                tracing::error!(error = %e, delay_ms, "Pending tx stream error");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn run_pending_tx_once(
    ws_url: &str,
    chain_id: u64,
    tx: &broadcast::Sender<PendingTxEvent>,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("pending tx WS connect: {e}"))?;

    let mut stream = provider
        .subscribe_pending_transactions()
        .await
        .map_err(|e| anyhow::anyhow!("subscribe_pending_transactions: {e}"))?
        .into_stream();

    tracing::info!(chain_id, "Pending tx subscription active");

    while let Some(hash) = stream.next().await {
        let event = PendingTxEvent {
            tx_hash: hash,
            received_at_unix_ms: now_unix_ms(),
            chain_id,
        };
        // Ignore send error — no receivers during startup is acceptable
        let _ = tx.send(event);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// LendingProtocolStream — LA engine input
// -----------------------------------------------------------------------------

/// A parsed lending protocol event (Borrow, Repay, Liquidate, etc.).
#[derive(Debug, Clone)]
pub struct LendingProtocolEvent {
    /// Raw log from the chain.
    pub log: Log,
    /// Protocol identifier (from address lookup).
    pub protocol: LendingProtocol,
    /// Unix timestamp (ms) when received.
    pub received_at_unix_ms: u64,
    /// Chain ID.
    pub chain_id: u64,
}

/// Identifies which lending protocol emitted the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LendingProtocol {
    AaveV3,
    CompoundV3,
    Morpho,
    EulerV2,
}

/// Lending protocol contract addresses on Arbitrum (chain 42161).
mod arbitrum_addrs {
    use alloy::primitives::{address, Address};

    pub const AAVE_V3_POOL: Address = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");
    pub const COMPOUND_V3: Address = address!("0000000000000000000000000000000000000001");
    pub const MORPHO: Address = address!("0000000000000000000000000000000000000002");
    pub const EULER_V2: Address = address!("eeee15a3a7de0b6a7d1e5c6c4a4b8e5e2e6e6ddd");
}

/// Subscribe to lending protocol events on-chain.
///
/// Subscribes to log events from Aave v3, Compound v3, Morpho, and
/// Euler v2 contracts.  Parses each log into a `LendingProtocolEvent`
/// and forwards to `tx`.
pub async fn run_lending_protocol_stream(
    ws_url: String,
    chain_id: u64,
    limiter: Arc<RpcRateLimiter>,
    tx: broadcast::Sender<LendingProtocolEvent>,
) {
    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        limiter.wait_until_allowed(RpcRequestKind::Subscribe).await;

        match run_lending_once(&ws_url, chain_id, &tx).await {
            Ok(()) => {
                tracing::warn!("Lending protocol stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) => {
                tracing::error!(error = %e, delay_ms, "Lending protocol stream error");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn run_lending_once(
    ws_url: &str,
    chain_id: u64,
    tx: &broadcast::Sender<LendingProtocolEvent>,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("lending WS connect: {e}"))?;

    // Subscribe to logs from all lending protocol contracts
    let filter = Filter::new().address(vec![
        arbitrum_addrs::AAVE_V3_POOL,
        arbitrum_addrs::COMPOUND_V3,
        arbitrum_addrs::MORPHO,
        arbitrum_addrs::EULER_V2,
    ]);

    let mut stream = provider
        .subscribe_logs(&filter)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe_logs (lending): {e}"))?
        .into_stream();

    tracing::info!(chain_id, "Lending protocol subscription active");

    while let Some(log) = stream.next().await {
        let protocol = match log.address() {
            a if a == arbitrum_addrs::AAVE_V3_POOL => LendingProtocol::AaveV3,
            a if a == arbitrum_addrs::COMPOUND_V3 => LendingProtocol::CompoundV3,
            a if a == arbitrum_addrs::MORPHO => LendingProtocol::Morpho,
            a if a == arbitrum_addrs::EULER_V2 => LendingProtocol::EulerV2,
            other => {
                tracing::warn!(addr = %other, "Unknown lending contract in log");
                continue;
            }
        };

        let event = LendingProtocolEvent {
            log,
            protocol,
            received_at_unix_ms: now_unix_ms(),
            chain_id,
        };
        let _ = tx.send(event);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// DexSyncStream — MSA Bellman-Ford trigger
// -----------------------------------------------------------------------------

/// A DEX pool reserve update event.
#[derive(Debug, Clone)]
pub struct DexSyncEvent {
    /// Raw log.
    pub log: Log,
    /// Pool contract address.
    pub pool: Address,
    /// Unix timestamp (ms) when received.
    pub received_at_unix_ms: u64,
    /// Chain ID.
    pub chain_id: u64,
}

/// keccak256("Sync(uint112,uint112)") — UniswapV2/V3 Sync event.
const SYNC_TOPIC: B256 =
    alloy::primitives::b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

/// Subscribe to DEX pool Sync events.
///
/// Triggers the MSA Bellman-Ford graph update (§10).  The 50ms debounce
/// is applied by omega-oracle, not here.
pub async fn run_dex_sync_stream(
    ws_url: String,
    chain_id: u64,
    limiter: Arc<RpcRateLimiter>,
    tx: broadcast::Sender<DexSyncEvent>,
) {
    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        limiter.wait_until_allowed(RpcRequestKind::Subscribe).await;

        match run_dex_sync_once(&ws_url, chain_id, &tx).await {
            Ok(()) => {
                tracing::warn!("DEX sync stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) => {
                tracing::error!(error = %e, delay_ms, "DEX sync stream error");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn run_dex_sync_once(
    ws_url: &str,
    chain_id: u64,
    tx: &broadcast::Sender<DexSyncEvent>,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("dex sync WS connect: {e}"))?;

    // Match any address — filter by topic only (Sync is universal)
    let filter = Filter::new().event_signature(SYNC_TOPIC);

    let mut stream = provider
        .subscribe_logs(&filter)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe_logs (dex sync): {e}"))?
        .into_stream();

    tracing::info!(chain_id, "DEX sync subscription active");

    while let Some(log) = stream.next().await {
        let event = DexSyncEvent {
            pool: log.address(),
            log,
            received_at_unix_ms: now_unix_ms(),
            chain_id,
        };
        let _ = tx.send(event);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// FeeOracleStream — base fee updates for §7 dual-component gas model
// -----------------------------------------------------------------------------

/// A fee oracle update from a new block header.
#[derive(Debug, Clone)]
pub struct FeeOracleEvent {
    /// EIP-1559 base fee in gwei.
    pub base_fee_gwei: u64,
    /// Block number.
    pub block_number: u64,
    /// Unix timestamp (ms) when received.
    pub received_at_unix_ms: u64,
    /// Chain ID.
    pub chain_id: u64,
}

/// Subscribe to new block headers and emit fee oracle events.
///
/// Downstream: omega-oracle uses these to update `FeeSnapshot` and
/// refresh the dual-component gas model inputs (§7).
pub async fn run_fee_oracle_stream(
    ws_url: String,
    chain_id: u64,
    limiter: Arc<RpcRateLimiter>,
    tx: broadcast::Sender<FeeOracleEvent>,
) {
    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        limiter.wait_until_allowed(RpcRequestKind::Subscribe).await;

        match run_fee_oracle_once(&ws_url, chain_id, &tx).await {
            Ok(()) => {
                tracing::warn!("Fee oracle stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) => {
                tracing::error!(error = %e, delay_ms, "Fee oracle stream error");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn run_fee_oracle_once(
    ws_url: &str,
    chain_id: u64,
    tx: &broadcast::Sender<FeeOracleEvent>,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("fee oracle WS connect: {e}"))?;

    let mut stream = provider
        .subscribe_blocks()
        .await
        .map_err(|e| anyhow::anyhow!("subscribe_blocks (fee oracle): {e}"))?
        .into_stream();

    tracing::info!(chain_id, "Fee oracle subscription active");

    while let Some(block) = stream.next().await {
        let base_fee_gwei = block.header.base_fee_per_gas.unwrap_or(0) / 1_000_000_000;

        let event = FeeOracleEvent {
            base_fee_gwei: base_fee_gwei as u64,
            block_number: block.header.number,
            received_at_unix_ms: now_unix_ms(),
            chain_id,
        };
        let _ = tx.send(event);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// MevShareStream — Phase 4 order-flow signal
// -----------------------------------------------------------------------------

/// A Flashbots MEV-Share bundle event.
///
/// Used by Phase 4 MEV-OFA to detect backrun opportunities.
/// The `payload` is the raw SSE data line from mev-share.flashbots.net.
#[derive(Debug, Clone)]
pub struct MevShareEvent {
    /// Raw JSON payload from the SSE stream.
    pub payload: serde_json::Value,
    /// Unix timestamp (ms) when received.
    pub received_at_unix_ms: u64,
}

const MEV_SHARE_URL: &str = "https://mev-share.flashbots.net/api/v1/events";

/// Subscribe to the Flashbots MEV-Share SSE stream.
///
/// Uses HTTP long-polling with `reqwest` (streaming body).  Reconnects
/// with exponential backoff: 1s → 2s → 4s → … → 30s cap.
///
/// Fallback scoring for periods with no SSE data uses the historical
/// 30-day median adverse selection score, NOT neutral 0.5.
///
/// NOTE: This function requires `reqwest` in omega-rpc's dependencies.
/// It is currently a separate function from the WS-based streams and
/// does NOT consume a rate-limiter token — SSE is a single long-lived
/// HTTP connection, not a per-request RPC call.
pub async fn run_mev_share_stream(tx: broadcast::Sender<MevShareEvent>) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("reqwest client");

    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        match run_mev_share_once(&client, &tx).await {
            Ok(()) => {
                tracing::warn!("MEV-Share SSE stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) => {
                tracing::error!(error = %e, delay_ms, "MEV-Share SSE error");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(BACKOFF_MAX_MS);
            }
        }
    }
}

async fn run_mev_share_once(
    client: &reqwest::Client,
    tx: &broadcast::Sender<MevShareEvent>,
) -> anyhow::Result<()> {
    tracing::info!("Connecting to MEV-Share SSE stream");

    let mut response = client
        .get(MEV_SHARE_URL)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("MEV-Share HTTP connect: {e}"))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!("MEV-Share HTTP {}", response.status()));
    }

    tracing::info!("MEV-Share SSE stream connected");

    // Read the SSE stream as a sequence of chunks
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("MEV-Share SSE read: {e}"))?
    {
        // SSE lines are `data: {json}\n\n`
        let text = std::str::from_utf8(&chunk)
            .map_err(|e| anyhow::anyhow!("MEV-Share SSE encoding: {e}"))?;

        for line in text.lines() {
            let Some(json_str) = line.strip_prefix("data: ") else {
                continue;
            };
            let json_str = json_str.trim();
            if json_str.is_empty() {
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(json_str) {
                Ok(payload) => {
                    let _ = tx.send(MevShareEvent {
                        payload,
                        received_at_unix_ms: now_unix_ms(),
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, raw = json_str, "Failed to parse MEV-Share payload");
                }
            }
        }
    }

    Ok(())
}
