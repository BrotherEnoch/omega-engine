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
//   Single WebSocket connection to a dedicated node (500 rps target),
//   shared across every call this client makes — `get_or_connect()`
//   lazily establishes it once and every subsequent call (block
//   subscription, `fetch_fee_snapshot`, `fetch_logs`,
//   `submit_raw_transaction`) reuses the same handle, only reconnecting
//   when the cached connection is explicitly invalidated after a
//   failure. This was NOT previously true despite the doc claiming it:
//   `fetch_fee_snapshot`/`fetch_logs` each opened a brand-new connection
//   on every single call, which both wasted a full WS-handshake's worth
//   of latency per call (a real competitive disadvantage for a
//   latency-sensitive strategy) and meant chain-ID verification (see
//   below) would have needed to be repeated, or skipped, on every call
//   rather than checked once at the actual trust boundary.
//
//   Reconnection is handled by `connect_with_retry` — exponential
//   backoff from 1s to 30s, EXCEPT for fatal (configuration) errors —
//   see `RpcClientError::is_fatal` in `net.rs` — which stop the retry
//   loop immediately instead of looping forever against something that
//   can never succeed.
//
// ## Chain ID verification (audit finding — previously missing entirely)
//
//   Every connection establishment — the initial `connect()`, and every
//   reconnect inside `get_or_connect()` — calls `eth_chainId` via
//   `net::verify_chain_id` and compares it against `config.chain_id`.
//   Nothing previously did this: a misconfigured or misrouted endpoint
//   would have silently mislabeled every block, log, and fee snapshot
//   with the wrong `chain_id` from that point forward. A mismatch is
//   fatal (non-retryable) — see `RpcClientError`.
//
// ## Transaction de-duplication (audit finding — previously absent entirely)
//
//   `submit_raw_transaction` is the ONLY intended path for submitting a
//   signed transaction. It tracks recently-submitted transaction hashes
//   in a bounded, TTL'd cache and refuses to resubmit a hash already
//   seen within the dedup window — WITHOUT making any network call or
//   consuming a write-rate-limit token for the rejected duplicate. This
//   guards specifically against a caller-side retry bug that mistakes a
//   slow-but-successful submission for a failure and resubmits it,
//   which without this guard could result in the same trade being
//   placed twice.
//
// ## Reorg / staleness signal (audit finding — previously absent)
//
//   Every `BlockEvent` now carries `is_reorg_or_stale`, computed by
//   comparing the incoming block's number against the highest number
//   previously observed on this subscription. Previously every block
//   was broadcast verbatim with no such check — a reorg, a duplicate
//   delivery, or a misbehaving endpoint serving stale data was
//   indistinguishable from genuine chain progress to every downstream
//   consumer.
//
// ## Health integration
//
//   `OmegaRpcClient` holds an optional `Arc<dyn LayerHealth>` for the
//   ExternalData layer. On any successful (re)connection the health
//   layer is set to Healthy UNCONDITIONALLY — previously this only
//   happened when recovering from a prior Degraded state specifically,
//   which meant a clean first-time connect never promoted health out of
//   its initial Unknown state (and, per the corresponding fix in
//   omega-core, Unknown is correctly treated as NOT operational — so
//   this bug could leave the engine perpetually believing RPC
//   connectivity was unavailable despite it working fine). On a fatal
//   (non-retryable) error, health is set to Halted, not Degraded — a
//   Degraded state implies "will recover on its own with retries,"
//   which is false for a configuration error.
//
// ## Logging (audit finding — credentials were being logged in the clear)
//
//   Most RPC providers embed an API key directly in the WS URL's path
//   or query string. Every log site that previously included the raw
//   `ws_url` now uses `net::redact_ws_url`, which keeps only the scheme
//   and host.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::B256;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Block, Filter, Log};
use futures::StreamExt;
use tokio::sync::{broadcast, Mutex};

use omega_core::{FeeSnapshot, HealthState, LayerHealth};

use crate::net::{redact_ws_url, validate_ws_scheme, verify_chain_id, wei_to_gwei_saturating, RpcClientError};
use crate::rate_limiter::{RpcRateLimiter, RpcRequestKind};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

const RECONNECT_DELAY_INITIAL_MS: u64 = 1_000;
const RECONNECT_DELAY_MAX_MS:     u64 = 30_000;
const BLOCK_CHANNEL_CAPACITY:  usize  = 64;

/// How long a submitted transaction hash is remembered for duplicate
/// detection. 30s comfortably covers a typical caller-side retry loop's
/// mistaken-failure window on Arbitrum (250ms blocks; a transaction
/// that's actually landing should confirm well within this).
const SUBMISSION_DEDUP_WINDOW: Duration = Duration::from_secs(30);

/// Hard upper bound on how many in-flight submission hashes are
/// tracked at once, so this cache can never grow unboundedly even
/// under a pathological submission rate — see `SubmissionTracker`.
const SUBMISSION_TRACKER_MAX_ENTRIES: usize = 10_000;

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
    /// True when `number` did not strictly increase relative to the
    /// highest block number previously observed on this subscription —
    /// signals a possible reorg, stale replay, or duplicate delivery
    /// from the RPC endpoint. Downstream consumers (e.g. the LA reorg
    /// guard) should treat any blueprint built against state at or
    /// after this event's block with extra caution rather than trusting
    /// it as the unconditional new chain tip.
    pub is_reorg_or_stale: bool,
}

impl BlockEvent {
    /// True when this block's timestamp is plausible relative to
    /// `now_unix_secs` — i.e. not further in the future than
    /// `max_future_skew_secs` allows.
    ///
    /// Deliberately does not bound how far in the PAST a timestamp may
    /// be: historical or replayed blocks legitimately have old
    /// timestamps. A timestamp further in the FUTURE than reasonable
    /// clock skew, however, indicates either a misbehaving RPC endpoint
    /// or a severe clock-sync problem, and should not be trusted
    /// silently. The tolerance is caller-supplied rather than hardcoded
    /// here, since the right tolerance is a deployment/config decision,
    /// not something this crate should assume.
    pub fn is_timestamp_plausible(&self, now_unix_secs: u64, max_future_skew_secs: u64) -> bool {
        self.timestamp <= now_unix_secs.saturating_add(max_future_skew_secs)
    }
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
    /// EIP-155 chain ID — used to stamp outbound signals AND verified
    /// against the connected endpoint's actual `eth_chainId` at every
    /// connection establishment (see module doc comment).
    pub chain_id:  u64,
}

impl RpcClientConfig {
    pub fn new(ws_url: impl Into<String>, rps_limit: u32, chain_id: u64) -> Self {
        Self { ws_url: ws_url.into(), rps_limit, chain_id }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SubmissionTracker
// ─────────────────────────────────────────────────────────────────────────────

/// Bounded, TTL-based de-duplication cache. Prevents the same signed
/// transaction hash from being submitted twice within `dedup_window` —
/// e.g. due to a caller-side retry loop mistakenly treating a slow-but-
/// successful submission as a failure and resubmitting it. See
/// `OmegaRpcClient::submit_raw_transaction`.
struct SubmissionTracker {
    recent: HashMap<B256, Instant>,
    dedup_window: Duration,
    max_tracked: usize,
}

impl SubmissionTracker {
    fn new(dedup_window: Duration, max_tracked: usize) -> Self {
        Self { recent: HashMap::new(), dedup_window, max_tracked }
    }

    /// Returns `true` if `hash` was already submitted within the dedup
    /// window (i.e. this submission would be a DUPLICATE and must be
    /// rejected). Also opportunistically evicts expired entries and
    /// enforces `max_tracked` as a hard bound, so this cache can never
    /// grow unbounded.
    fn check_and_record(&mut self, hash: B256, now: Instant) -> bool {
        self.recent
            .retain(|_, submitted_at| now.duration_since(*submitted_at) < self.dedup_window);

        if self.recent.contains_key(&hash) {
            return true; // duplicate
        }

        if self.recent.len() >= self.max_tracked {
            // Fails safe by NOT falsely reporting a duplicate for a
            // new, legitimate hash — but logs loudly, since dedup
            // protection is now degraded for new submissions until
            // enough entries age out. This should never happen under
            // realistic submission rates (max 1 write/cycle, §4) —
            // hitting this bound indicates something upstream is
            // submitting far faster than the engine's own design
            // assumes, which is itself worth knowing about.
            tracing::error!(
                tracked = self.recent.len(),
                max = self.max_tracked,
                "submission tracker at capacity — duplicate detection degraded for \
                 new hashes until existing entries expire"
            );
            return false;
        }

        self.recent.insert(hash, now);
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ConnectionState
// ─────────────────────────────────────────────────────────────────────────────

type SharedProvider = Arc<dyn Provider>;

struct ConnectionState {
    provider: Option<SharedProvider>,
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaRpcClient
// ─────────────────────────────────────────────────────────────────────────────

/// Rate-limited WebSocket RPC client for the Omega Engine.
///
/// Wraps a single shared alloy provider connection with:
///   - Token-bucket rate limiting per request kind (§22 hardware spec)
///   - Block header broadcast channel for downstream consumers, with
///     reorg/staleness flagging
///   - Health layer integration for ExternalData transitions
///   - Automatic reconnect with exponential backoff, chain-ID-verified
///     on every (re)connection
///   - Transaction submission de-duplication
///
/// Cloning is cheap — all fields are `Arc`-wrapped.
#[derive(Clone)]
pub struct OmegaRpcClient {
    config:             RpcClientConfig,
    rate_limiter:       RpcRateLimiter,
    block_tx:           broadcast::Sender<BlockEvent>,
    health:             Option<Arc<dyn LayerHealth>>,
    connection:         Arc<Mutex<ConnectionState>>,
    submission_tracker: Arc<Mutex<SubmissionTracker>>,
}

impl OmegaRpcClient {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Connect to the given WebSocket endpoint.
    ///
    /// Validates the URL scheme, establishes the connection, and
    /// verifies the endpoint's actual chain ID matches
    /// `config.chain_id` before returning — a `RpcClientError` from
    /// this method is `is_fatal()` for both a bad URL and a chain-ID
    /// mismatch, since retrying either with the same configuration can
    /// never succeed. Use `connect_with_retry` when the caller can
    /// tolerate transient initial-connection failure but should still
    /// stop immediately on a fatal one.
    pub async fn connect(config: RpcClientConfig) -> Result<Self, RpcClientError> {
        validate_ws_scheme(&config.ws_url)?;

        let provider = open_provider(&config.ws_url).await?;
        verify_chain_id(provider.as_ref(), config.chain_id).await?;

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
            ws_url   = %redact_ws_url(&config.ws_url),
            chain_id = config.chain_id,
            rps      = config.rps_limit,
            "OmegaRpcClient connected",
        );

        Ok(Self {
            config,
            rate_limiter: limiter,
            block_tx,
            health: None,
            connection: Arc::new(Mutex::new(ConnectionState { provider: Some(provider) })),
            submission_tracker: Arc::new(Mutex::new(SubmissionTracker::new(
                SUBMISSION_DEDUP_WINDOW,
                SUBMISSION_TRACKER_MAX_ENTRIES,
            ))),
        })
    }

    /// Connect with exponential backoff retry.
    ///
    /// Retries on TRANSIENT failures (network blips, node restarts) —
    /// backoff 1s → 2s → 4s → … → 30s cap. Stops IMMEDIATELY and
    /// returns `Err` on a FATAL error (bad URL scheme, chain-ID
    /// mismatch), since retrying those with the same configuration can
    /// never succeed — looping forever against a permanent
    /// misconfiguration previously gave an operator no signal that the
    /// problem needed their attention rather than more patience.
    pub async fn connect_with_retry(config: RpcClientConfig) -> Result<Self, RpcClientError> {
        let mut delay_ms = RECONNECT_DELAY_INITIAL_MS;
        loop {
            match Self::connect(config.clone()).await {
                Ok(client) => return Ok(client),
                Err(e) if e.is_fatal() => {
                    tracing::error!(
                        error = %e,
                        ws_url = %redact_ws_url(&config.ws_url),
                        "RPC connect failed with a FATAL, non-retryable error — giving up. \
                         This requires operator intervention (check RPC URL / chain_id config).",
                    );
                    return Err(e);
                }
                Err(e) => {
                    tracing::warn!(
                        error    = %e,
                        delay_ms,
                        ws_url   = %redact_ws_url(&config.ws_url),
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

    // ── Connection management ─────────────────────────────────────────────────

    /// Returns the current shared provider connection, establishing
    /// (and chain-ID-verifying) a new one if none is cached or the
    /// cached one was invalidated by a previous failure.
    ///
    /// This is the single place every RPC call in this client goes
    /// through to get a connection — `fetch_fee_snapshot`,
    /// `fetch_logs`, `submit_raw_transaction`, and the block
    /// subscription loop all share whatever connection is cached here,
    /// rather than each independently opening its own.
    pub async fn get_or_connect(&self) -> anyhow::Result<Arc<dyn Provider>> {
        self.get_or_connect_typed().await.map_err(anyhow::Error::from)
    }

    async fn get_or_connect_typed(&self) -> Result<Arc<dyn Provider>, RpcClientError> {
        let mut state = self.connection.lock().await;
        if let Some(p) = &state.provider {
            return Ok(p.clone());
        }
        let provider = open_provider(&self.config.ws_url).await?;
        verify_chain_id(provider.as_ref(), self.config.chain_id).await?;
        state.provider = Some(provider.clone());
        Ok(provider)
    }

    /// Drops the cached connection so the next call to
    /// `get_or_connect`/`get_or_connect_typed` re-establishes it rather
    /// than silently reusing a connection that just errored or whose
    /// underlying stream ended.
    pub async fn invalidate_connection(&self) {
        let mut state = self.connection.lock().await;
        state.provider = None;
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    /// Subscribe to new block headers.
    pub fn subscribe_blocks(&self) -> broadcast::Receiver<BlockEvent> {
        self.block_tx.subscribe()
    }

    /// Start the block header subscription background task.
    ///
    /// Must be spawned as a Tokio task. Reconnects automatically with
    /// backoff on a transient error. Returns ONLY when a fatal
    /// (non-retryable) error occurs — a configuration problem
    /// (misconfigured URL, chain-ID mismatch) that retrying can never
    /// fix. The caller/supervisor of the spawned task should treat task
    /// completion as needing operator attention; under normal operation
    /// (transient errors, clean reconnects) this never returns.
    pub async fn run_block_subscription(&self) {
        let mut delay_ms = RECONNECT_DELAY_INITIAL_MS;
        loop {
            match self.run_block_subscription_once().await {
                Ok(()) => {
                    tracing::warn!("Block header stream ended — reconnecting");
                    delay_ms = RECONNECT_DELAY_INITIAL_MS;
                }
                Err(e) if e.is_fatal() => {
                    tracing::error!(
                        error = %e,
                        "Block header stream: FATAL non-retryable error — stopping subscription. \
                         This requires operator intervention (check RPC URL / chain_id config).",
                    );
                    if let Some(ref health) = self.health {
                        health.set_state(HealthState::Halted, &format!("fatal RPC error: {e}"));
                    }
                    return;
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

    async fn run_block_subscription_once(&self) -> Result<(), RpcClientError> {
        self.rate_limiter
            .wait_until_allowed(RpcRequestKind::Subscribe)
            .await;

        let provider = match self.get_or_connect_typed().await {
            Ok(p) => p,
            Err(e) => {
                self.invalidate_connection().await;
                return Err(e);
            }
        };

        // Set Healthy UNCONDITIONALLY on a successful (re)connect —
        // previously this only fired when recovering from Degraded
        // specifically, so a clean first-time connect never promoted
        // health out of its initial Unknown state.
        if let Some(ref health) = self.health {
            health.set_state(HealthState::Healthy, "RPC block stream connected");
        }

        let mut stream = provider
            .subscribe_blocks()
            .await
            .map_err(|e| RpcClientError::ConnectFailed(format!("subscribe_blocks failed: {e}")))?
            .into_stream();

        tracing::info!("Block header subscription active");

        let mut last_block_number: Option<u64> = None;

        while let Some(block) = stream.next().await {
            let event = block_to_event(&block, last_block_number);

            if event.is_reorg_or_stale {
                tracing::warn!(
                    block_number = event.number,
                    last_seen    = ?last_block_number,
                    "Block number did not strictly increase — possible reorg or \
                     stale/duplicate data from RPC endpoint; flagged for downstream \
                     reorg handling",
                );
            }
            last_block_number = Some(event.number.max(last_block_number.unwrap_or(0)));

            tracing::debug!(
                block_number = event.number,
                hash         = %event.hash,
                base_fee     = ?event.base_fee_gwei,
                is_reorg_or_stale = event.is_reorg_or_stale,
                "New block",
            );

            if self.block_tx.send(event).is_err() {
                tracing::debug!("No active subscribers for block events");
            }
        }

        // Stream ended — the connection is no longer usable. Invalidate
        // it so the next call (whether this same subscription loop, or
        // fetch_fee_snapshot/fetch_logs/submit_raw_transaction running
        // concurrently) reconnects rather than silently reusing a dead
        // handle.
        self.invalidate_connection().await;

        Ok(())
    }

    // ── Rate-limited calls ────────────────────────────────────────────────────

    async fn wait_for_token(
        &self,
        kind: RpcRequestKind,
        wait_timeout: Option<Duration>,
    ) -> anyhow::Result<()> {
        match wait_timeout {
            Some(timeout) => {
                tokio::time::timeout(timeout, self.rate_limiter.wait_until_allowed(kind))
                    .await
                    .map_err(|_| anyhow::anyhow!("RPC {kind} rate-limit timeout"))?;
            }
            None => {
                self.rate_limiter.wait_until_allowed(kind).await;
            }
        }
        Ok(())
    }

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
        self.wait_for_token(RpcRequestKind::Read, wait_timeout).await?;
        f().await
    }

    /// Execute a write after consuming a rate-limit token.
    ///
    /// For submitting a SIGNED TRANSACTION specifically, prefer
    /// `submit_raw_transaction` instead — it additionally guards
    /// against double-submission of the same transaction hash, which
    /// this generic method has no awareness of.
    pub async fn gated_write<F, Fut, T>(
        &self,
        wait_timeout: Option<Duration>,
        f: F,
    ) -> anyhow::Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        self.wait_for_token(RpcRequestKind::Write, wait_timeout).await?;
        f().await
    }

    /// Submit a raw signed transaction, guarded by BOTH the write-rate
    /// limiter AND a de-duplication check against `tx_hash` (the
    /// transaction's own hash).
    ///
    /// If `tx_hash` was already submitted within the last 30 seconds,
    /// this returns `Err` IMMEDIATELY — without consuming a
    /// write-rate-limit token and without calling `send` at all. This
    /// is the guard against a caller-side retry bug that mistakes a
    /// slow-but-successful submission for a failure and resubmits the
    /// identical transaction, which without this guard could place the
    /// same trade twice.
    ///
    /// `send` should perform the actual `eth_sendRawTransaction` call
    /// (via `self.get_or_connect()` internally, same as
    /// `fetch_fee_snapshot`/`fetch_logs`) and is only invoked when
    /// `tx_hash` is not a recent duplicate.
    pub async fn submit_raw_transaction<F, Fut>(
        &self,
        tx_hash: B256,
        wait_timeout: Option<Duration>,
        send: F,
    ) -> anyhow::Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        {
            let mut tracker = self.submission_tracker.lock().await;
            if tracker.check_and_record(tx_hash, Instant::now()) {
                anyhow::bail!(
                    "duplicate submission rejected: tx_hash {tx_hash} was already \
                     submitted within the last {:?} — refusing to resubmit",
                    SUBMISSION_DEDUP_WINDOW,
                );
            }
        }

        self.wait_for_token(RpcRequestKind::Write, wait_timeout).await?;
        send().await
    }

    // ── Fee oracle ────────────────────────────────────────────────────────────

    /// Fetch the current fee snapshot from the node, using the shared
    /// connection (see `get_or_connect`).
    pub async fn fetch_fee_snapshot(&self) -> anyhow::Result<FeeSnapshot> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;

            let block = provider
                .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest, false)
                .await
                .map_err(|e| anyhow::anyhow!("eth_getBlockByNumber failed: {e}"))?
                .ok_or_else(|| anyhow::anyhow!("latest block not found"))?;

            // Saturating conversion — see net::wei_to_gwei_saturating for
            // why a bare `as u64` cast here is dangerous (silently
            // wraps an absurd/malformed base fee into a small, WRONG,
            // artificially-cheap-looking gas cost).
            let base_fee_gwei =
                wei_to_gwei_saturating(block.header.base_fee_per_gas.unwrap_or(0) as u128);

            Ok(FeeSnapshot {
                base_fee_gwei,
                l1_data_fee_gwei:  0,
                priority_fee_gwei: 0,
                block_number:      block.header.number,
            })
        })
        .await
    }

    /// Fetch logs matching a filter, rate-limited as a read, using the
    /// shared connection.
    pub async fn fetch_logs(&self, filter: Filter) -> anyhow::Result<Vec<Log>> {
        self.gated_read(None, || async move {
            let provider = self.get_or_connect().await?;
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

async fn open_provider(ws_url: &str) -> Result<Arc<dyn Provider>, RpcClientError> {
    let provider = ProviderBuilder::new()
        .on_builtin(ws_url)
        .await
        .map_err(|e| RpcClientError::ConnectFailed(format!("WS connect: {e}")))?;
    Ok(Arc::new(provider))
}

fn block_to_event(block: &Block, last_block_number: Option<u64>) -> BlockEvent {
    let base_fee_gwei = block
        .header
        .base_fee_per_gas
        .map(|fee| wei_to_gwei_saturating(fee as u128));

    let is_reorg_or_stale = match last_block_number {
        Some(last) => block.header.number <= last,
        None => false,
    };

    BlockEvent {
        number: block.header.number,
        hash: block.header.hash,
        base_fee_gwei,
        timestamp: block.header.timestamp,
        is_reorg_or_stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_block(number: u64) -> Block {
        // NOTE: this constructs the minimal fields these tests need via
        // whatever Default/builder path alloy's Block/Header type
        // exposes in this workspace's alloy version. Kept intentionally
        // minimal — full Block construction details are alloy-version-
        // specific and this test only needs `header.number` to vary.
        let mut block = Block::default();
        block.header.number = number;
        block
    }

    #[test]
    fn block_to_event_flags_non_increasing_block_number() {
        let event = block_to_event(&sample_block(100), Some(100));
        assert!(event.is_reorg_or_stale, "same block number again must be flagged");

        let event2 = block_to_event(&sample_block(99), Some(100));
        assert!(event2.is_reorg_or_stale, "a lower block number must be flagged");
    }

    #[test]
    fn block_to_event_does_not_flag_genuine_progress() {
        let event = block_to_event(&sample_block(101), Some(100));
        assert!(!event.is_reorg_or_stale);
    }

    #[test]
    fn block_to_event_first_block_is_never_flagged() {
        let event = block_to_event(&sample_block(1), None);
        assert!(!event.is_reorg_or_stale, "no prior block to compare against");
    }

    #[test]
    fn is_timestamp_plausible_rejects_far_future() {
        let event = BlockEvent {
            number: 1,
            hash: B256::ZERO,
            base_fee_gwei: None,
            timestamp: 10_000,
            is_reorg_or_stale: false,
        };
        assert!(event.is_timestamp_plausible(9_990, 30), "within tolerance");
        assert!(!event.is_timestamp_plausible(9_000, 30), "1000s in the future, tolerance 30s");
    }

    #[test]
    fn is_timestamp_plausible_allows_arbitrarily_old() {
        let event = BlockEvent {
            number: 1,
            hash: B256::ZERO,
            base_fee_gwei: None,
            timestamp: 100,
            is_reorg_or_stale: false,
        };
        assert!(event.is_timestamp_plausible(1_000_000, 30), "old timestamps are always plausible");
    }

    #[test]
    fn submission_tracker_rejects_duplicate_within_window() {
        let mut tracker = SubmissionTracker::new(Duration::from_secs(30), 100);
        let hash = B256::from([0x11u8; 32]);
        let t0 = Instant::now();

        assert!(!tracker.check_and_record(hash, t0), "first submission is not a duplicate");
        assert!(tracker.check_and_record(hash, t0), "second submission within window IS a duplicate");
    }

    #[test]
    fn submission_tracker_allows_resubmission_after_window_expires() {
        let mut tracker = SubmissionTracker::new(Duration::from_millis(10), 100);
        let hash = B256::from([0x22u8; 32]);
        let t0 = Instant::now();

        assert!(!tracker.check_and_record(hash, t0));
        let later = t0 + Duration::from_millis(50);
        assert!(
            !tracker.check_and_record(hash, later),
            "after the dedup window expires, the same hash is no longer treated as a duplicate"
        );
    }

    #[test]
    fn submission_tracker_distinguishes_different_hashes() {
        let mut tracker = SubmissionTracker::new(Duration::from_secs(30), 100);
        let t0 = Instant::now();
        let hash_a = B256::from([0xAAu8; 32]);
        let hash_b = B256::from([0xBBu8; 32]);

        assert!(!tracker.check_and_record(hash_a, t0));
        assert!(!tracker.check_and_record(hash_b, t0), "a different hash is never a duplicate");
    }

    #[test]
    fn submission_tracker_enforces_max_capacity() {
        let mut tracker = SubmissionTracker::new(Duration::from_secs(3600), 2);
        let t0 = Instant::now();
        let h1 = B256::from([0x01u8; 32]);
        let h2 = B256::from([0x02u8; 32]);
        let h3 = B256::from([0x03u8; 32]);

        assert!(!tracker.check_and_record(h1, t0));
        assert!(!tracker.check_and_record(h2, t0));
        // At capacity now (2 tracked, long TTL so nothing expires) —
        // a third, genuinely-new hash must NOT be falsely reported as
        // a duplicate; dedup protection is simply degraded for it.
        assert!(
            !tracker.check_and_record(h3, t0),
            "at capacity, a new hash must not be falsely flagged as a duplicate"
        );
    }
}