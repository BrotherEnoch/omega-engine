// crates/omega-rpc/src/subscriptions.rs
//
// On-chain subscription streams for the Omega Engine signal pipeline.
//
// Each stream: acquires a subscribe token, opens a chain-ID-verified WS
// connection via `ws_provider`, publishes parsed events to a broadcast
// channel, and reconnects with backoff on error — except FATAL errors
// (bad URL, chain-ID mismatch; see `net::RpcClientError::is_fatal`),
// which stop the loop immediately rather than retrying forever against
// something that can never succeed. This mirrors client.rs's pattern.
//
// Audit fixes in this pass:
//   - `ws_provider` now validates URL scheme and verifies chain_id on
//     every (re)connect — previously nothing in this file checked either.
//   - `run_lending_protocol_stream` refuses to start unless chain_id is
//     42161 (the hardcoded addresses are Arbitrum-specific) AND refuses
//     to start while COMPOUND_V3/MORPHO/EULER_V2 remain the placeholder
//     addresses they were in the reviewed source (0x...0001, 0x...0002,
//     and an unverified Euler address). Inventing plausible-looking
//     replacements would be worse than refusing to run — see
//     `arbitrum_addrs` below for what must be supplied first.
//   - `run_fee_oracle_once` uses `wei_to_gwei_saturating` instead of a
//     truncating cast (see net.rs for why that matters).
//   - `run_mev_share_stream` now buffers bytes across HTTP chunks and
//     only processes complete lines — the previous per-chunk parsing
//     silently dropped or corrupted any SSE event that straddled a
//     chunk boundary (data chunking has no awareness of line or even
//     UTF-8 character boundaries), with a bounded buffer size so a
//     misbehaving server can't grow it unboundedly.
//   - (external audit response) every broadcast send now goes through
//     `send_or_log`, which logs a warning when there are zero active
//     receivers instead of silently discarding the event via
//     `let _ = tx.send(...)`. A trading engine that stops receiving
//     signals because its consumer crashed or fell behind, while the
//     producer loop keeps running and looks healthy, is exactly the
//     kind of silent failure worth surfacing. This does NOT add retry
//     or buffering — it's an observability fix, not a delivery
//     guarantee.
//   - (external audit response) `run_lending_once` and
//     `run_dex_sync_once` now skip (and warn on) any `Log` the RPC
//     marked `removed: true` — logs the node itself is telling us were
//     orphaned by a reorg. Forwarding a removed log as a live signal
//     could send the LA/MSA engines chasing a liquidation or arb
//     opportunity that the chain has already un-happened.
//   - (this revision) removed an unnecessary `as u128` cast in
//     `run_fee_oracle_once` — `block.header.base_fee_per_gas` is
//     already `Option<u128>` here, so `.unwrap_or(0) as u128` was
//     casting u128 to u128. Functionally harmless, but flagged by
//     clippy's `unnecessary_cast` lint under this workspace's
//     `-D warnings` clippy invocation.
//
// ## Known follow-ups NOT fixed in this pass (flagged, not silently skipped)
//
// An external audit of this file also raised these; they're valid, but
// each needs infrastructure (a shared health/watchdog service, a
// cross-provider dedup cache, a stateful subscription manager) that
// doesn't exist in this crate today, so a same-file patch here would be
// either fake or would invent a design the rest of the system hasn't
// agreed to:
//   - Freshness / staleness watchdog: nothing here notices a WS
//     connection that stays open but stops delivering messages (a dead
//     TCP session with no FIN). A `last_message_at` timestamp per
//     stream, checked by an external health monitor, would catch this;
//     that monitor and its polling contract live outside this crate.
//   - Subscription identity re-verification after reconnect: chain_id
//     IS re-verified on every reconnect (`ws_provider`), which catches
//     the dangerous "silently pointed at the wrong chain" case. Full
//     subscription-ID continuity checking across a provider-side
//     reconnect/rebalance is a reasonable further hardening step for a
//     stateful subscription manager, not a stateless per-call helper
//     like the ones in this file.
//   - Cross-provider replay/duplicate-event detection (`already_seen`
//     by tx_hash/log_index/block+tx+log): a real gap for exactly the
//     reason raised — a provider replaying already-delivered events on
//     reconnect could produce duplicate downstream signals. This
//     belongs in a shared dedup cache keyed the same way across all
//     signal sources (mempool, logs, MEV-Share), not duplicated
//     per-stream in this file; `omega_relay::dedup::PositionKey` and
//     `ExecutionBlueprint::idempotency_key` (omega-core) already
//     provide dedup at later pipeline stages, but neither operates at
//     the raw-event layer these streams produce.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log};
use futures::StreamExt;
use tokio::sync::broadcast;

use crate::net::{validate_ws_scheme, verify_chain_id, wei_to_gwei_saturating, RpcClientError};
use crate::rate_limiter::{RpcRateLimiter, RpcRequestKind};

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default() // clock-before-epoch: falls back to 0, i.e.
        // "maximally stale" — the safe direction for any staleness check.
        .as_millis() as u64
}

const BACKOFF_INITIAL_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;

/// Connects and verifies chain_id in one step — every subscription
/// function in this file goes through this rather than connecting
/// directly, so chain-ID verification can't be skipped at one call site
/// while present at another.
async fn ws_provider(
    ws_url: &str,
    expected_chain_id: u64,
) -> Result<impl Provider, RpcClientError> {
    validate_ws_scheme(ws_url)?;
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("WS connect: {e}")))?;
    verify_chain_id(&provider, expected_chain_id).await?;
    Ok(provider)
}

/// Sends an event on a broadcast channel, logging (rather than
/// silently discarding) the case where there are no active receivers.
///
/// `broadcast::Sender::send` returns `Err` exactly when there are zero
/// live `Receiver`s — the previous code everywhere in this file did
/// `let _ = tx.send(...)`, which drops that signal on the floor. For a
/// trading engine, "the signal pipeline has no listener" and "the
/// signal pipeline is healthy but idle" are very different states, and
/// only this function's caller can currently tell them apart — so it
/// logs it rather than continuing to look healthy while producing
/// events nobody consumes.
///
/// This is deliberately just a log, not a retry/buffer/backpressure
/// mechanism: broadcast channels are fire-and-forget by design, and
/// adding delivery guarantees here would be a bigger design change
/// than an audit-response patch should make unilaterally.
fn send_or_log<T>(tx: &broadcast::Sender<T>, event: T, stream: &'static str) {
    if tx.send(event).is_err() {
        tracing::warn!(
            stream,
            "broadcast send: no active receivers — event dropped"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PendingTxStream — SA / MSA order flow
// ─────────────────────────────────────────────────────────────────────────────

/// A pending transaction observed in the mempool.
#[derive(Debug, Clone)]
pub struct PendingTxEvent {
    pub tx_hash: B256,
    pub received_at_unix_ms: u64,
    pub chain_id: u64,
}

/// Subscribe to pending transactions and forward to `tx`. Runs
/// indefinitely; reconnects on transient error, stops on a fatal one.
/// Spawn as a Tokio task.
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
            Err(e) if e.is_fatal() => {
                tracing::error!(error = %e, "Pending tx stream: fatal error, stopping");
                return;
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
) -> Result<(), RpcClientError> {
    let provider = ws_provider(ws_url, chain_id).await?;

    let mut stream = provider
        .subscribe_pending_transactions()
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("subscribe_pending_transactions: {e}")))?
        .into_stream();

    tracing::info!(chain_id, "Pending tx subscription active");

    while let Some(hash) = stream.next().await {
        send_or_log(
            tx,
            PendingTxEvent {
                tx_hash: hash,
                received_at_unix_ms: now_unix_ms(),
                chain_id,
            },
            "pending_tx",
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// LendingProtocolStream — LA engine input
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed lending protocol event.
#[derive(Debug, Clone)]
pub struct LendingProtocolEvent {
    pub log: Log,
    pub protocol: LendingProtocol,
    pub received_at_unix_ms: u64,
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

    /// Real, verified: Aave V3 Pool on Arbitrum.
    pub const AAVE_V3_POOL: Address = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");

    /// UNVERIFIED PLACEHOLDERS — not real deployed contracts. Replace
    /// with the verified current addresses for Compound V3's Comet
    /// contract, Morpho's market contract, and Euler V2's vault/factory
    /// on Arbitrum before enabling this stream. `run_lending_once`
    /// refuses to start while these remain unset (see
    /// `ensure_addresses_configured`) rather than silently subscribing
    /// to the wrong (or nonexistent) contracts.
    pub const COMPOUND_V3: Address = address!("0000000000000000000000000000000000000001");
    pub const MORPHO: Address = address!("0000000000000000000000000000000000000002");
    pub const EULER_V2: Address = address!("eeee15a3a7de0b6a7d1e5c6c4a4b8e5e2e6e6ddd");

    pub const PLACEHOLDER_ADDRESSES: [Address; 3] = [COMPOUND_V3, MORPHO, EULER_V2];
}

const LENDING_STREAM_CHAIN_ID: u64 = 42161; // addresses above are Arbitrum-specific

fn ensure_lending_addresses_configured() -> anyhow::Result<()> {
    if arbitrum_addrs::PLACEHOLDER_ADDRESSES.iter().any(|_| true) {
        // Always true today — see doc comment on PLACEHOLDER_ADDRESSES.
        // Kept as an explicit, named check (rather than silently
        // deleting it once addresses are real) so the moment real
        // addresses replace the placeholders, someone updates this
        // list to remove the ones now verified, re-enabling the stream
        // for exactly the ones that are ready.
        if !arbitrum_addrs::PLACEHOLDER_ADDRESSES.is_empty() {
            anyhow::bail!(
                "refusing to start: {} contract address(es) (COMPOUND_V3, MORPHO, EULER_V2) \
                 are unverified placeholders, not real deployed contracts — see \
                 arbitrum_addrs's doc comment for what must be supplied first",
                arbitrum_addrs::PLACEHOLDER_ADDRESSES.len()
            );
        }
    }
    Ok(())
}

/// Subscribe to lending protocol events on-chain. Refuses to start (see
/// module doc comment) if `chain_id != 42161` or if placeholder
/// addresses are still configured.
pub async fn run_lending_protocol_stream(
    ws_url: String,
    chain_id: u64,
    limiter: Arc<RpcRateLimiter>,
    tx: broadcast::Sender<LendingProtocolEvent>,
) {
    if chain_id != LENDING_STREAM_CHAIN_ID {
        tracing::error!(
            chain_id,
            expected = LENDING_STREAM_CHAIN_ID,
            "run_lending_protocol_stream refuses to start: hardcoded addresses are \
             Arbitrum-specific"
        );
        return;
    }
    if let Err(e) = ensure_lending_addresses_configured() {
        tracing::error!(error = %e, "run_lending_protocol_stream refuses to start");
        return;
    }

    let mut delay_ms = BACKOFF_INITIAL_MS;
    loop {
        limiter.wait_until_allowed(RpcRequestKind::Subscribe).await;
        match run_lending_once(&ws_url, chain_id, &tx).await {
            Ok(()) => {
                tracing::warn!("Lending protocol stream ended — reconnecting");
                delay_ms = BACKOFF_INITIAL_MS;
            }
            Err(e) if e.is_fatal() => {
                tracing::error!(error = %e, "Lending protocol stream: fatal error, stopping");
                return;
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
) -> Result<(), RpcClientError> {
    let provider = ws_provider(ws_url, chain_id).await?;

    let filter = Filter::new().address(vec![
        arbitrum_addrs::AAVE_V3_POOL,
        arbitrum_addrs::COMPOUND_V3,
        arbitrum_addrs::MORPHO,
        arbitrum_addrs::EULER_V2,
    ]);

    let mut stream = provider
        .subscribe_logs(&filter)
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("subscribe_logs (lending): {e}")))?
        .into_stream();

    tracing::info!(chain_id, "Lending protocol subscription active");

    while let Some(log) = stream.next().await {
        // A reorg can orphan a log the node already delivered; the RPC
        // marks it `removed: true` rather than silently un-sending it.
        // Forwarding it anyway would let the LA engine chase a
        // liquidation opportunity the chain no longer has.
        if log.removed {
            tracing::warn!(
                chain_id,
                block_number = ?log.block_number,
                tx_hash = ?log.transaction_hash,
                "Lending protocol log removed by reorg — not forwarding"
            );
            continue;
        }

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
        send_or_log(
            tx,
            LendingProtocolEvent {
                log,
                protocol,
                received_at_unix_ms: now_unix_ms(),
                chain_id,
            },
            "lending_protocol",
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// DexSyncStream — MSA Bellman-Ford trigger
// ─────────────────────────────────────────────────────────────────────────────

/// A DEX pool reserve update event.
#[derive(Debug, Clone)]
pub struct DexSyncEvent {
    pub log: Log,
    pub pool: Address,
    pub received_at_unix_ms: u64,
    pub chain_id: u64,
}

/// keccak256("Sync(uint112,uint112)") — UniswapV2 Sync event topic.
const SYNC_TOPIC: B256 =
    alloy::primitives::b256!("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

/// Subscribe to DEX pool Sync events.
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
            Err(e) if e.is_fatal() => {
                tracing::error!(error = %e, "DEX sync stream: fatal error, stopping");
                return;
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
) -> Result<(), RpcClientError> {
    let provider = ws_provider(ws_url, chain_id).await?;
    let filter = Filter::new().event_signature(SYNC_TOPIC);

    let mut stream = provider
        .subscribe_logs(&filter)
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("subscribe_logs (dex sync): {e}")))?
        .into_stream();

    tracing::info!(chain_id, "DEX sync subscription active");

    while let Some(log) = stream.next().await {
        // Same reorg-safety reasoning as run_lending_once: don't forward
        // a Sync event the node has already told us was orphaned.
        if log.removed {
            tracing::warn!(
                chain_id,
                block_number = ?log.block_number,
                pool = %log.address(),
                "DEX sync log removed by reorg — not forwarding"
            );
            continue;
        }

        send_or_log(
            tx,
            DexSyncEvent {
                pool: log.address(),
                log,
                received_at_unix_ms: now_unix_ms(),
                chain_id,
            },
            "dex_sync",
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// FeeOracleStream — base fee updates for §7 dual-component gas model
// ─────────────────────────────────────────────────────────────────────────────

/// A fee oracle update from a new block header.
#[derive(Debug, Clone)]
pub struct FeeOracleEvent {
    pub base_fee_gwei: u64,
    pub block_number: u64,
    pub received_at_unix_ms: u64,
    pub chain_id: u64,
}

/// Subscribe to new block headers and emit fee oracle events.
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
            Err(e) if e.is_fatal() => {
                tracing::error!(error = %e, "Fee oracle stream: fatal error, stopping");
                return;
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
) -> Result<(), RpcClientError> {
    let provider = ws_provider(ws_url, chain_id).await?;

    let mut stream = provider
        .subscribe_blocks()
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("subscribe_blocks (fee oracle): {e}")))?
        .into_stream();

    tracing::info!(chain_id, "Fee oracle subscription active");

    while let Some(block) = stream.next().await {
        // Saturating, not truncating — see net::wei_to_gwei_saturating.
        // No `as u128` here: base_fee_per_gas is already Option<u128>
        // on this Block type, so `.unwrap_or(0)` already yields u128.
        let base_fee_gwei = wei_to_gwei_saturating(block.header.base_fee_per_gas.unwrap_or(0));
        send_or_log(
            tx,
            FeeOracleEvent {
                base_fee_gwei,
                block_number: block.header.number,
                received_at_unix_ms: now_unix_ms(),
                chain_id,
            },
            "fee_oracle",
        );
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// MevShareStream — Phase 4 order-flow signal
// ─────────────────────────────────────────────────────────────────────────────

/// A Flashbots MEV-Share bundle event.
#[derive(Debug, Clone)]
pub struct MevShareEvent {
    pub payload: serde_json::Value,
    pub received_at_unix_ms: u64,
}

const MEV_SHARE_URL: &str = "https://mev-share.flashbots.net/api/v1/events";

/// Buffer bound: refuse to keep growing the cross-chunk line buffer
/// past this without finding a newline. Guards against unbounded memory
/// growth from a misbehaving server — a process also carrying real
/// trading state should never let an external stream grow memory
/// without limit.
const MAX_SSE_LINE_BUFFER_BYTES: usize = 1_000_000;

/// Subscribe to the Flashbots MEV-Share SSE stream. Reconnects with
/// exponential backoff on SSE drop.
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

    // Byte buffer across HTTP chunks — a chunk boundary has no
    // awareness of line or UTF-8 character boundaries, so a naive
    // "parse each chunk independently" approach silently drops or
    // corrupts any SSE event whose bytes straddle two chunks. Instead
    // we accumulate bytes here and only pull out and process complete
    // (`\n`-terminated) lines, leaving any trailing partial line in the
    // buffer for the next chunk.
    let mut buffer: Vec<u8> = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("MEV-Share SSE read: {e}"))?
    {
        buffer.extend_from_slice(&chunk);

        if buffer.len() > MAX_SSE_LINE_BUFFER_BYTES {
            return Err(anyhow::anyhow!(
                "MEV-Share SSE line buffer exceeded {} bytes without a newline — \
                 refusing to grow further",
                MAX_SSE_LINE_BUFFER_BYTES
            ));
        }

        // Process every complete line currently in the buffer; any
        // trailing partial line (no newline yet) is left behind for
        // the next chunk. `\n` is single-byte in UTF-8 and never
        // appears inside a multi-byte sequence, so splitting on it is
        // always safe even if a chunk boundary landed mid-character
        // somewhere else in the line.
        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let line = line.trim_end_matches(['\r', '\n']);

            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(data) {
                    Ok(payload) => {
                        send_or_log(
                            tx,
                            MevShareEvent {
                                payload,
                                received_at_unix_ms: now_unix_ms(),
                            },
                            "mev_share",
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "MEV-Share SSE: failed to parse event JSON");
                    }
                }
            }
        }
    }

    Ok(())
}
