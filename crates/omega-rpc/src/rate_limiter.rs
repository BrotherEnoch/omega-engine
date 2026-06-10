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
//   `wait_until_allowed` sleeps 5ms per retry.  This is intentional:
//   on a 500 rps budget the sleep duration is short enough to keep
//   overall latency below 10ms while preventing a tight spin that
//   would waste CPU.  The Tokio executor is not blocked — it is an
//   async sleep.
//
// ## Thread safety
//
//   `RpcRateLimiter` is `Clone` and `Send + Sync`.  The inner state
//   is wrapped in `Arc<Mutex<…>>`.  The Mutex is async (Tokio) — it
//   never blocks an OS thread.

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
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();

        // elapsed == 0.0 on the first call in the same millisecond
        if elapsed <= 0.0 {
            return;
        }

        let added = elapsed * self.refill_rate;
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
    /// Uses the computed time-until-available to sleep efficiently rather
    /// than polling on a fixed 5ms interval — this reduces unnecessary
    /// wakeups under load.  Falls back to a minimum 1ms sleep to avoid
    /// zero-duration sleeps that waste CPU without yielding.
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
                // Token is available — try to consume
                if self.allow(kind).await {
                    return;
                }
                // Race: another waiter consumed it first; retry immediately
                continue;
            }

            // Sleep for the computed duration (minimum 1ms)
            let sleep_ms = ((wait_secs * 1000.0).ceil() as u64).max(1);
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }
    }

    /// Telemetry snapshot of current bucket state and cumulative counts.
    pub async fn snapshot(&self) -> RateLimiterSnapshot {
        let mut g = self.inner.lock().await;
        RateLimiterSnapshot {
            read_tokens: g.read.available(),
            write_tokens: g.write.available(),
            subscribe_tokens: g.subscribe.available(),
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
    /// Available write tokens.
    pub write_tokens: f64,
    /// Available subscribe tokens.
    pub subscribe_tokens: f64,
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
    ///
    /// Used by the health monitor to detect RPC rate exhaustion.
    pub fn is_healthy(&self) -> bool {
        self.read_tokens > 1.0 && self.write_tokens > 1.0
    }

    /// RPC headroom metric (shadow scorecard `rpc_headroom`).
    ///
    /// = read_tokens / read_capacity_estimate.
    /// We use 400 as the capacity estimate (default config).
    /// Returns 1.0 when the bucket is full; 0.0 when empty.
    pub fn rpc_headroom(&self) -> f64 {
        (self.read_tokens / 400.0).clamp(0.0, 1.0)
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
        // wait_until_allowed must return within 5ms at 1000 rps
        tokio::time::timeout(
            Duration::from_millis(50),
            rl.wait_until_allowed(RpcRequestKind::Read),
        )
        .await
        .expect("wait_until_allowed must not time out");
    }

    #[tokio::test]
    async fn rpc_headroom_full_bucket() {
        let rl = RpcRateLimiter::new();
        let snap = rl.snapshot().await;
        assert!((snap.rpc_headroom() - 1.0).abs() < 0.01);
    }
}
