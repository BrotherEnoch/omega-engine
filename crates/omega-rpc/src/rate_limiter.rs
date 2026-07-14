// crates/omega-rpc/src/rate_limiter.rs
//
// Token-bucket rate limiter for the RPC layer.
//
// ## Spec constraints (§22, hardware spec)
//
//   Target throughput: 500 rps against a dedicated high-throughput node.
//
// ## Per-request-kind budgets
//
//   Read:      400 capacity, 400 refill/s  (80% of 500 rps budget)
//   Write:      50 capacity,  50 refill/s  (max 1 write/cycle — conservative)
//   Subscribe:  20 capacity,  20 refill/s  (subscriptions are long-lived; low churn)
//
// ## Microtx constraint (§4)
//
//   Max 8 reads per Microtx blueprint.  At 400 reads/s with ~50µs per
//   blueprint, this is comfortably within budget.  The limiter is the
//   hard safety rail; the Microtx path enforces the 8-read budget
//   independently.
//
// ## Backpressure
//
//   `wait_until_allowed` computes the exact time until a token becomes
//   available and sleeps for that duration (minimum 1ms) rather than
//   polling on a fixed interval — this keeps latency low without a
//   tight spin.  The Tokio executor is not blocked — it is an async
//   sleep.
//
// ## Thread safety
//
//   `RpcRateLimiter` is `Clone` and `Send + Sync`.  The inner state
//   is wrapped in `Arc<Mutex<…>>`.  The Mutex is async (Tokio) — it
//   never blocks an OS thread.
//
// ## Audit finding fixed in this pass
//
// `RateLimiterSnapshot::rpc_headroom()` previously hardcoded an assumed
// read-bucket capacity of 400 in its calculation. `OmegaRpcClient::connect`
// (client.rs) constructs CUSTOM bucket capacities from
// `config.rps_limit` whenever it's nonzero — meaning any client
// configured with an `rps_limit` other than the exact value that
// produces a 400-capacity read bucket would silently get a WRONG
// headroom fraction from this method: too generous (risking an
// unexpected real rate-limit hit at the worst time) or too conservative
// (risking throttling legitimate trading activity for no real reason),
// depending on direction. Fixed by tracking and reporting the actual
// configured capacity in every snapshot rather than assuming one.

use std::time::{Duration, Instant};

use std::sync::Arc;
use tokio::sync::Mutex;

// ─────────────────────────────────────────────────────────────────────────────
// RpcRequestKind
// ─────────────────────────────────────────────────────────────────────────────

/// Classification of an RPC call for rate-limiter bucket selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RpcRequestKind {
    /// `eth_call`, `eth_getBalance`, `eth_getStorageAt`, block/receipt
    /// queries.  Up to 8 per Microtx blueprint (§4).
    Read,

    /// `eth_sendRawTransaction`.  Max 1 per execution cycle.
    Write,

    /// `eth_subscribe` (new block headers, logs, pending txs).
    /// Long-lived connections — low churn.
    Subscribe,
}

impl std::fmt::Display for RpcRequestKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcRequestKind::Read => f.write_str("read"),
            RpcRequestKind::Write => f.write_str("write"),
            RpcRequestKind::Subscribe => f.write_str("subscribe"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BucketConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for a single token bucket.
#[derive(Debug, Clone)]
pub struct BucketConfig {
    /// Maximum number of tokens (burst capacity).
    pub capacity: u32,
    /// Tokens added per second (steady-state rate).
    pub refill_per_second: u32,
}

impl BucketConfig {
    /// Default read bucket: 400 capacity, 400 rps.
    pub fn read_default() -> Self {
        Self {
            capacity: 400,
            refill_per_second: 400,
        }
    }

    /// Default write bucket: 50 capacity, 50 rps.
    pub fn write_default() -> Self {
        Self {
            capacity: 50,
            refill_per_second: 50,
        }
    }

    /// Default subscribe bucket: 20 capacity, 20 rps.
    pub fn subscribe_default() -> Self {
        Self {
            capacity: 20,
            refill_per_second: 20,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TokenBucket
// ─────────────────────────────────────────────────────────────────────────────

/// Continuous-time token bucket with fractional token accumulation.
///
/// Tokens accumulate at `refill_rate` tokens/second.  The bucket never
/// exceeds `capacity`.  Each call to `try_consume` deducts one token
/// atomically (under the outer Mutex).
struct TokenBucket {
    capacity: u32,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            capacity,
            tokens: capacity as f64, // start full
            refill_rate: refill_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    /// Accumulate tokens for time elapsed since the last call.
    ///
    /// Uses `Instant`, which is monotonic — this cannot go backwards
    /// even if the system wall clock is adjusted, so `elapsed` can
    /// never be negative here in practice; the `<= 0.0` guard below
    /// only handles the same-instant (zero-elapsed) case, not clock
    /// skew.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();

        // elapsed == 0.0 on the first call in the same millisecond
        if elapsed <= 0.0 {
            return;
        }

        let added = elapsed * self.refill_rate;
        // Clamping to `capacity` on every refill call bounds any
        // floating-point drift over a long process uptime — tokens can
        // never accumulate past capacity regardless of how many refill
        // calls have happened, so imprecision can't compound unbounded.
        self.tokens = (self.tokens + added).min(self.capacity as f64);
        self.last_refill = now;
    }

    /// Attempt to consume one token.  Returns `true` on success.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Current available tokens (after refill).
    fn available(&mut self) -> f64 {
        self.refill();
        self.tokens
    }

    /// This bucket's configured capacity.
    fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Time until at least one token is available, in seconds.
    ///
    /// Returns 0.0 when a token is already available.
    fn time_until_available(&mut self) -> f64 {
        self.refill();
        if self.tokens >= 1.0 {
            return 0.0;
        }
        // Need (1.0 - tokens) more tokens at refill_rate tokens/sec
        (1.0 - self.tokens) / self.refill_rate
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RpcRateLimiter
// ─────────────────────────────────────────────────────────────────────────────

/// Async-safe token-bucket rate limiter for the RPC layer.
///
/// `Clone` — all clones share the same underlying state via `Arc`.
/// Safe to hold across `.await` points.
#[derive(Clone)]
pub struct RpcRateLimiter {
    inner: Arc<Mutex<InnerLimiter>>,
}

struct InnerLimiter {
    read: TokenBucket,
    write: TokenBucket,
    subscribe: TokenBucket,
    /// Cumulative counts for telemetry
    total_reads: u64,
    total_writes: u64,
    total_subscribes: u64,
    total_throttled: u64,
}

impl RpcRateLimiter {
    /// Create with default production buckets (400/50/20 rps).
    pub fn new() -> Self {
        Self::with_config(
            BucketConfig::read_default(),
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        )
    }

    /// Create with explicit bucket configurations.
    pub fn with_config(read: BucketConfig, write: BucketConfig, subscribe: BucketConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InnerLimiter {
                read: TokenBucket::new(read.capacity, read.refill_per_second),
                write: TokenBucket::new(write.capacity, write.refill_per_second),
                subscribe: TokenBucket::new(subscribe.capacity, subscribe.refill_per_second),
                total_reads: 0,
                total_writes: 0,
                total_subscribes: 0,
                total_throttled: 0,
            })),
        }
    }

    /// Attempt to consume one token without waiting.
    ///
    /// Returns `true` when the request is allowed immediately.
    /// Returns `false` when the bucket is empty (throttled).
    pub async fn allow(&self, kind: RpcRequestKind) -> bool {
        let mut g = self.inner.lock().await;
        let allowed = match kind {
            RpcRequestKind::Read => g.read.try_consume(),
            RpcRequestKind::Write => g.write.try_consume(),
            RpcRequestKind::Subscribe => g.subscribe.try_consume(),
        };
        if allowed {
            match kind {
                RpcRequestKind::Read => g.total_reads += 1,
                RpcRequestKind::Write => g.total_writes += 1,
                RpcRequestKind::Subscribe => g.total_subscribes += 1,
            }
        } else {
            g.total_throttled += 1;
            tracing::debug!(kind = %kind, "RPC request throttled");
        }
        allowed
    }

    /// Wait until a token of `kind` is available, then consume it.
    ///
    /// Computes the exact time-until-available and sleeps for that
    /// duration rather than polling on a fixed interval — this reduces
    /// unnecessary wakeups under load while keeping latency low.  Falls
    /// back to a minimum 1ms sleep to avoid zero-duration sleeps that
    /// waste CPU without yielding.
    ///
    /// The time-until-available value is read under one lock
    /// acquisition and then re-checked via `allow()` under a SEPARATE
    /// acquisition — there is a real time-of-check-to-time-of-use gap
    /// between those two lock holds if multiple callers race for the
    /// same bucket. This is handled correctly, not accidentally: if
    /// another waiter consumes the token first, `allow()`'s own
    /// `try_consume()` call (which re-checks state fresh, under its own
    /// lock) simply returns `false`, and the loop retries rather than
    /// assuming the earlier read is still valid.
    pub async fn wait_until_allowed(&self, kind: RpcRequestKind) {
        loop {
            let wait_secs = {
                let mut g = self.inner.lock().await;
                let bucket = match kind {
                    RpcRequestKind::Read => &mut g.read,
                    RpcRequestKind::Write => &mut g.write,
                    RpcRequestKind::Subscribe => &mut g.subscribe,
                };
                bucket.time_until_available()
            };

            if wait_secs <= 0.0 {
                // Token appeared available as of the read above — try
                // to consume it now.
                if self.allow(kind).await {
                    return;
                }
                // Race: another waiter consumed it first between our
                // read and our consume attempt; retry immediately
                // rather than sleeping, since a token may already be
                // available again.
                continue;
            }

            // Sleep for the computed duration (minimum 1ms)
            let sleep_ms = ((wait_secs * 1000.0).ceil() as u64).max(1);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
    }

    /// Telemetry snapshot of current bucket state, configured
    /// capacities, and cumulative counts.
    pub async fn snapshot(&self) -> RateLimiterSnapshot {
        let mut g = self.inner.lock().await;
        RateLimiterSnapshot {
            read_tokens: g.read.available(),
            read_capacity: g.read.capacity(),
            write_tokens: g.write.available(),
            write_capacity: g.write.capacity(),
            subscribe_tokens: g.subscribe.available(),
            subscribe_capacity: g.subscribe.capacity(),
            total_reads: g.total_reads,
            total_writes: g.total_writes,
            total_subscribes: g.total_subscribes,
            total_throttled: g.total_throttled,
        }
    }
}

impl Default for RpcRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RateLimiterSnapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Point-in-time telemetry snapshot of the rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiterSnapshot {
    /// Available read tokens (after refill).
    pub read_tokens: f64,
    /// The read bucket's CONFIGURED capacity — tracked explicitly
    /// rather than assumed, since `OmegaRpcClient::connect` can
    /// construct a non-default capacity from `config.rps_limit`.
    pub read_capacity: u32,
    /// Available write tokens.
    pub write_tokens: f64,
    /// The write bucket's configured capacity.
    pub write_capacity: u32,
    /// Available subscribe tokens.
    pub subscribe_tokens: f64,
    /// The subscribe bucket's configured capacity.
    pub subscribe_capacity: u32,
    /// Cumulative read requests allowed.
    pub total_reads: u64,
    /// Cumulative write requests allowed.
    pub total_writes: u64,
    /// Cumulative subscribe requests allowed.
    pub total_subscribes: u64,
    /// Cumulative requests throttled (any kind).
    pub total_throttled: u64,
}

impl RateLimiterSnapshot {
    /// Returns `true` when both read and write buckets have capacity.
    pub fn is_healthy(&self) -> bool {
        self.read_tokens > 1.0 && self.write_tokens > 1.0
    }

    /// RPC headroom metric (shadow scorecard `rpc_headroom`):
    /// fraction of the READ bucket's ACTUAL configured capacity
    /// currently available.
    ///
    /// Uses `read_capacity` (the real configured value) rather than a
    /// hardcoded assumption — see this file's module-level audit note
    /// for why that distinction matters: a client configured with a
    /// non-default `rps_limit` previously produced a silently wrong
    /// headroom fraction, which is dangerous in either direction if
    /// anything downstream makes throttling or alerting decisions from
    /// it.
    pub fn rpc_headroom(&self) -> f64 {
        if self.read_capacity == 0 {
            return 0.0;
        }
        (self.read_tokens / self.read_capacity as f64).clamp(0.0, 1.0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_limiter_allows_immediately() {
        let rl = RpcRateLimiter::new();
        assert!(rl.allow(RpcRequestKind::Read).await);
        assert!(rl.allow(RpcRequestKind::Write).await);
        assert!(rl.allow(RpcRequestKind::Subscribe).await);
    }

    #[tokio::test]
    async fn depleted_read_bucket_throttles() {
        // Tiny bucket: capacity 2
        let rl = RpcRateLimiter::with_config(
            BucketConfig {
                capacity: 2,
                refill_per_second: 1,
            },
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        );
        assert!(rl.allow(RpcRequestKind::Read).await);
        assert!(rl.allow(RpcRequestKind::Read).await);
        // Third request should be throttled
        assert!(!rl.allow(RpcRequestKind::Read).await);
    }

    #[tokio::test]
    async fn write_throttle_does_not_affect_reads() {
        let rl = RpcRateLimiter::with_config(
            BucketConfig::read_default(),
            BucketConfig {
                capacity: 1,
                refill_per_second: 1,
            },
            BucketConfig::subscribe_default(),
        );
        rl.allow(RpcRequestKind::Write).await; // consume the 1 write token
        assert!(!rl.allow(RpcRequestKind::Write).await); // write throttled
        assert!(rl.allow(RpcRequestKind::Read).await); // reads unaffected
    }

    #[tokio::test]
    async fn snapshot_counts_requests() {
        let rl = RpcRateLimiter::new();
        rl.allow(RpcRequestKind::Read).await;
        rl.allow(RpcRequestKind::Read).await;
        rl.allow(RpcRequestKind::Write).await;

        let snap = rl.snapshot().await;
        assert_eq!(snap.total_reads, 2);
        assert_eq!(snap.total_writes, 1);
    }

    #[tokio::test]
    async fn snapshot_counts_throttled() {
        let rl = RpcRateLimiter::with_config(
            BucketConfig {
                capacity: 1,
                refill_per_second: 100,
            },
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        );
        rl.allow(RpcRequestKind::Read).await; // allowed
        rl.allow(RpcRequestKind::Read).await; // throttled

        let snap = rl.snapshot().await;
        assert_eq!(snap.total_throttled, 1);
    }

    #[tokio::test]
    async fn snapshot_is_healthy_with_full_buckets() {
        let rl = RpcRateLimiter::new();
        let snap = rl.snapshot().await;
        assert!(snap.is_healthy());
    }

    #[tokio::test]
    async fn wait_until_allowed_returns_after_refill() {
        // 2-token bucket at 1000 rps — refills very fast
        let rl = RpcRateLimiter::with_config(
            BucketConfig {
                capacity: 2,
                refill_per_second: 1_000,
            },
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        );
        // Drain the bucket
        rl.allow(RpcRequestKind::Read).await;
        rl.allow(RpcRequestKind::Read).await;
        // wait_until_allowed must return within 50ms at 1000 rps
        tokio::time::timeout(
            Duration::from_millis(50),
            rl.wait_until_allowed(RpcRequestKind::Read),
        )
        .await
        .expect("wait_until_allowed must not time out");
    }

    #[tokio::test]
    async fn rpc_headroom_full_bucket_default_capacity() {
        let rl = RpcRateLimiter::new();
        let snap = rl.snapshot().await;
        assert_eq!(snap.read_capacity, 400);
        assert!((snap.rpc_headroom() - 1.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn rpc_headroom_reflects_custom_capacity_not_hardcoded_400() {
        // Regression test for the bug this pass fixes: a limiter
        // configured with a non-default capacity (as
        // OmegaRpcClient::connect does whenever rps_limit != the exact
        // value that happens to produce a 400-capacity read bucket)
        // must report headroom relative to ITS OWN capacity, not a
        // hardcoded 400.
        let rl = RpcRateLimiter::with_config(
            BucketConfig { capacity: 800, refill_per_second: 800 }, // custom, NOT 400
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        );

        // Full bucket: headroom must read as 1.0 (full), not 2.0
        // (which the old hardcoded-400 formula would have produced
        // before being clamped, silently masking that the bucket
        // wasn't actually full relative to ITS capacity in other
        // scenarios).
        let snap_full = rl.snapshot().await;
        assert_eq!(snap_full.read_capacity, 800);
        assert!((snap_full.rpc_headroom() - 1.0).abs() < 0.01);

        // Drain to exactly half of the 800 capacity (400 consumed).
        for _ in 0..400 {
            rl.allow(RpcRequestKind::Read).await;
        }
        let snap_half = rl.snapshot().await;
        // Correct answer relative to real capacity: ~0.5.
        // The old hardcoded-400 formula would have computed
        // (400 remaining / 400 hardcoded) = 1.0 — falsely reporting
        // "full headroom" when the bucket was actually half-drained
        // relative to its real 800 capacity.
        assert!(
            (snap_half.rpc_headroom() - 0.5).abs() < 0.05,
            "expected ~0.5 headroom relative to real capacity 800, got {}",
            snap_half.rpc_headroom()
        );
    }

    #[tokio::test]
    async fn rpc_headroom_zero_capacity_does_not_divide_by_zero() {
        let rl = RpcRateLimiter::with_config(
            BucketConfig { capacity: 0, refill_per_second: 0 },
            BucketConfig::write_default(),
            BucketConfig::subscribe_default(),
        );
        let snap = rl.snapshot().await;
        assert_eq!(snap.rpc_headroom(), 0.0);
    }
}