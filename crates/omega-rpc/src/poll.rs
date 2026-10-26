// crates/omega-rpc/src/poll.rs
//
// Polling primitives for the HTTP transport path (no eth_subscribe).
//
// ## Contents
//
//   Backoff            exponential backoff with jitter (used on HTTP 429)
//   decide_tip         pure decision logic: given the last emitted block and the
//                      current chain tip, which blocks must be emitted?
//   LogCursor          pure planning of eth_getLogs ranges (chunking, lookback cap)
//   fetch_logs_split   eth_getLogs over a range, splitting on "range too large"
//   run_log_poller     the shared log-polling loop used by the lending and DEX streams
//   absorb_error       one place that decides "back off and keep going" vs "give up"
//
// ## Known limitations (inherent to polling, stated rather than hidden)
//
//   - Log polling reads up to `head - OMEGA_LOGS_CONFIRMATIONS` and never replays a
//     range. A reorg that replaces blocks whose logs were already emitted is NOT
//     re-fetched. With confirmations = 0 (default) that window is small on Arbitrum
//     but real; raise OMEGA_LOGS_CONFIRMATIONS if you need it closed.
//   - A single block whose logs exceed the provider's result cap cannot be split
//     further; it is skipped with an ERROR log (those logs are lost).
//   - After a long outage only the most recent LOGS_MAX_LOOKBACK blocks of logs are
//     recovered; the gap is logged at WARN.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log};
use tokio::time::MissedTickBehavior;

use crate::net::{classify_rpc_error, scrub_urls, RpcClientError, RpcErrorKind};
use crate::rate_limiter::{RpcRateLimiter, RpcRequestKind};
use crate::transport::PollConfig;

/// Most blocks of logs recovered after a stall; anything older is skipped.
const LOGS_MAX_LOOKBACK: u64 = 5_000;

/// Upper bound on eth_getLogs chunks processed in a single poll tick while catching up.
const MAX_LOG_CHUNKS_PER_TICK: usize = 20;

// ─────────────────────────────────────────────────────────────────────────────
// Backoff
// ─────────────────────────────────────────────────────────────────────────────

fn jitter_ms(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0)
        % bound
}

/// Exponential backoff with up to +25% jitter, so several pollers hitting the same
/// 429 do not retry in lockstep.
#[derive(Debug, Clone)]
pub(crate) struct Backoff {
    current_ms: u64,
    initial_ms: u64,
    max_ms: u64,
}

impl Backoff {
    pub(crate) fn new(initial_ms: u64, max_ms: u64) -> Self {
        Self {
            current_ms: initial_ms,
            initial_ms,
            max_ms,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.current_ms = self.initial_ms;
    }

    pub(crate) fn next_delay(&mut self) -> Duration {
        let base = self.current_ms;
        let delay = base + jitter_ms(base / 4 + 1);
        self.current_ms = base.saturating_mul(2).min(self.max_ms);
        Duration::from_millis(delay)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error absorption
// ─────────────────────────────────────────────────────────────────────────────

/// Decides what a polling failure means.
///
/// - Rate-limited (HTTP 429 etc.): sleeps with backoff and returns `Ok(())` so the
///   caller simply continues its loop.
/// - Anything else: returns `Err`, so the caller's reconnect loop takes over.
///
/// The message is passed through `scrub_urls` first — reqwest errors embed the full
/// request URL, which commonly contains an API key.
pub(crate) async fn absorb_error(
    stream: &'static str,
    msg: String,
    backoff: &mut Backoff,
) -> Result<(), RpcClientError> {
    let msg = scrub_urls(&msg);
    match classify_rpc_error(&msg) {
        RpcErrorKind::RateLimited => {
            let delay = backoff.next_delay();
            tracing::warn!(
                stream,
                delay_ms = delay.as_millis() as u64,
                error = %msg,
                "RPC rate-limited — backing off"
            );
            tokio::time::sleep(delay).await;
            Ok(())
        }
        _ => Err(RpcClientError::ConnectFailed(format!("{stream}: {msg}"))),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Block tip decision
// ─────────────────────────────────────────────────────────────────────────────

/// What the block poller should do after observing the current chain tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TipAction {
    /// Same block as already emitted, or an older one (lagging load-balanced node).
    Nothing,
    /// Emit the tip, preceded by `backfill` (inclusive range) when blocks were missed.
    /// `skipped` counts blocks that were missed but are too old to backfill.
    Emit {
        backfill: Option<(u64, u64)>,
        skipped: u64,
    },
}

/// Pure decision logic for the block poller.
///
/// `last` is the (number, hash) of the last block emitted. `max_catchup` bounds how
/// many blocks (tip included) may be emitted in one go after a stall.
///
/// A same-height block with a DIFFERENT hash is a tip reorg and is emitted (the
/// caller flags it, because its number does not exceed the last emitted number).
pub(crate) fn decide_tip(
    last: Option<(u64, B256)>,
    number: u64,
    hash: B256,
    max_catchup: u64,
) -> TipAction {
    let Some((last_n, last_h)) = last else {
        return TipAction::Emit {
            backfill: None,
            skipped: 0,
        };
    };

    if number < last_n {
        return TipAction::Nothing;
    }
    if number == last_n {
        return if hash == last_h {
            TipAction::Nothing
        } else {
            TipAction::Emit {
                backfill: None,
                skipped: 0,
            }
        };
    }

    let gap = number - last_n - 1;
    if gap == 0 {
        return TipAction::Emit {
            backfill: None,
            skipped: 0,
        };
    }
    let take = gap.min(max_catchup.saturating_sub(1));
    let skipped = gap - take;
    let backfill = if take == 0 {
        None
    } else {
        Some((number - take, number - 1))
    };
    TipAction::Emit { backfill, skipped }
}

// ─────────────────────────────────────────────────────────────────────────────
// Log range planning
// ─────────────────────────────────────────────────────────────────────────────

/// One eth_getLogs range to query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LogPlan {
    pub from: u64,
    pub to: u64,
    /// Blocks dropped because the cursor fell further behind than LOGS_MAX_LOOKBACK.
    pub skipped: u64,
}

/// Tracks the next block whose logs have not been fetched yet.
#[derive(Debug, Clone)]
pub(crate) struct LogCursor {
    next_from: Option<u64>,
    max_range: u64,
    max_lookback: u64,
}

impl LogCursor {
    pub(crate) fn new(max_range: u64) -> Self {
        Self {
            next_from: None,
            max_range: max_range.max(1),
            max_lookback: LOGS_MAX_LOOKBACK,
        }
    }

    /// Next range to fetch, or `None` when already caught up to `head - confirmations`.
    /// The first call starts at the safe head (no historical backfill).
    pub(crate) fn plan(&self, head: u64, confirmations: u64) -> Option<LogPlan> {
        let safe = head.saturating_sub(confirmations);
        let mut from = self.next_from.unwrap_or(safe);
        if from > safe {
            return None;
        }
        let mut skipped = 0;
        if safe - from > self.max_lookback {
            let new_from = safe - self.max_lookback;
            skipped = new_from - from;
            from = new_from;
        }
        let to = safe.min(from.saturating_add(self.max_range - 1));
        Some(LogPlan { from, to, skipped })
    }

    /// Marks everything up to and including `to` as fetched.
    pub(crate) fn advance(&mut self, to: u64) {
        self.next_from = Some(to.saturating_add(1));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// eth_getLogs with range splitting
// ─────────────────────────────────────────────────────────────────────────────

/// Fetches logs for `[from, to]`, halving the range whenever the provider says it is
/// too large. Results are returned in ascending block order.
///
/// A rate-limit error is returned to the caller (which backs off). A single block that
/// still exceeds the provider's cap is skipped with an ERROR log.
pub(crate) async fn fetch_logs_split(
    provider: &dyn Provider,
    limiter: &RpcRateLimiter,
    base: &Filter,
    from: u64,
    to: u64,
) -> Result<Vec<Log>, RpcClientError> {
    let mut out: Vec<Log> = Vec::new();
    let mut stack: Vec<(u64, u64)> = vec![(from, to)];

    while let Some((a, b)) = stack.pop() {
        limiter.wait_until_allowed(RpcRequestKind::Read).await;
        let filter = base.clone().from_block(a).to_block(b);
        match provider.get_logs(&filter).await {
            Ok(mut logs) => out.append(&mut logs),
            Err(e) => {
                let msg = scrub_urls(&e.to_string());
                match classify_rpc_error(&msg) {
                    RpcErrorKind::RangeTooLarge if b > a => {
                        let mid = a + (b - a) / 2;
                        // Push the later half first so the earlier half is popped first.
                        stack.push((mid + 1, b));
                        stack.push((a, mid));
                    }
                    RpcErrorKind::RangeTooLarge => {
                        tracing::error!(
                            block = a,
                            error = %msg,
                            "single block exceeds the provider's eth_getLogs result limit — \
                             its logs are SKIPPED"
                        );
                    }
                    _ => {
                        return Err(RpcClientError::ConnectFailed(format!(
                            "eth_getLogs({a}..{b}): {msg}"
                        )));
                    }
                }
            }
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared log poller
// ─────────────────────────────────────────────────────────────────────────────

/// Polls `base_filter` over successive block ranges and hands each log to `on_log`.
///
/// Never returns `Ok`; returns `Err` only for non-rate-limit failures, after which the
/// caller's reconnect loop restarts it (with a fresh cursor starting at the tip).
pub(crate) async fn run_log_poller<F>(
    provider: &dyn Provider,
    limiter: &RpcRateLimiter,
    cfg: &PollConfig,
    base_filter: Filter,
    stream: &'static str,
    mut on_log: F,
) -> Result<(), RpcClientError>
where
    F: FnMut(Log),
{
    let mut cursor = LogCursor::new(cfg.logs_max_range);
    let mut backoff = Backoff::new(1_000, 30_000);
    let mut tick = tokio::time::interval(cfg.logs_every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    tracing::info!(
        stream,
        every_ms = cfg.logs_every.as_millis() as u64,
        max_range = cfg.logs_max_range,
        confirmations = cfg.logs_confirmations,
        "Log polling active (HTTP transport)"
    );

    loop {
        tick.tick().await;

        limiter.wait_until_allowed(RpcRequestKind::Read).await;
        let head = match provider.get_block_number().await {
            Ok(h) => h,
            Err(e) => {
                absorb_error(stream, e.to_string(), &mut backoff).await?;
                continue;
            }
        };

        for _ in 0..MAX_LOG_CHUNKS_PER_TICK {
            let Some(plan) = cursor.plan(head, cfg.logs_confirmations) else {
                break;
            };
            if plan.skipped > 0 {
                tracing::warn!(
                    stream,
                    skipped_blocks = plan.skipped,
                    "log poller fell too far behind — logs for the skipped blocks are LOST"
                );
            }
            match fetch_logs_split(provider, limiter, &base_filter, plan.from, plan.to).await {
                Ok(logs) => {
                    backoff.reset();
                    for log in logs {
                        on_log(log);
                    }
                    cursor.advance(plan.to);
                }
                Err(e) => {
                    absorb_error(stream, e.to_string(), &mut backoff).await?;
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> B256 {
        B256::from([b; 32])
    }

    #[test]
    fn tip_first_block_is_emitted_without_backfill() {
        assert_eq!(
            decide_tip(None, 100, h(1), 50),
            TipAction::Emit {
                backfill: None,
                skipped: 0
            }
        );
    }

    #[test]
    fn tip_same_block_is_nothing() {
        assert_eq!(
            decide_tip(Some((100, h(1))), 100, h(1), 50),
            TipAction::Nothing
        );
    }

    #[test]
    fn tip_older_block_is_nothing() {
        assert_eq!(
            decide_tip(Some((100, h(1))), 99, h(9), 50),
            TipAction::Nothing
        );
    }

    #[test]
    fn tip_same_height_different_hash_is_emitted_as_reorg() {
        assert_eq!(
            decide_tip(Some((100, h(1))), 100, h(2), 50),
            TipAction::Emit {
                backfill: None,
                skipped: 0
            }
        );
    }

    #[test]
    fn tip_next_block_has_no_backfill() {
        assert_eq!(
            decide_tip(Some((100, h(1))), 101, h(2), 50),
            TipAction::Emit {
                backfill: None,
                skipped: 0
            }
        );
    }

    #[test]
    fn tip_small_gap_is_fully_backfilled() {
        assert_eq!(
            decide_tip(Some((10, h(1))), 14, h(2), 50),
            TipAction::Emit {
                backfill: Some((11, 13)),
                skipped: 0
            }
        );
    }

    #[test]
    fn tip_large_gap_is_capped_and_reports_skipped() {
        // gap = 89, catch-up 5 → tip + 4 backfilled blocks, 85 skipped.
        assert_eq!(
            decide_tip(Some((10, h(1))), 100, h(2), 5),
            TipAction::Emit {
                backfill: Some((96, 99)),
                skipped: 85
            }
        );
    }

    #[test]
    fn tip_catchup_of_one_means_tip_only() {
        assert_eq!(
            decide_tip(Some((10, h(1))), 20, h(2), 1),
            TipAction::Emit {
                backfill: None,
                skipped: 9
            }
        );
    }

    #[test]
    fn log_cursor_starts_at_safe_head() {
        let c = LogCursor::new(1_000);
        assert_eq!(
            c.plan(500, 0),
            Some(LogPlan {
                from: 500,
                to: 500,
                skipped: 0
            })
        );
    }

    #[test]
    fn log_cursor_respects_confirmations() {
        let c = LogCursor::new(1_000);
        assert_eq!(
            c.plan(500, 3),
            Some(LogPlan {
                from: 497,
                to: 497,
                skipped: 0
            })
        );
    }

    #[test]
    fn log_cursor_idles_when_caught_up_then_resumes() {
        let mut c = LogCursor::new(1_000);
        c.advance(500);
        assert_eq!(c.plan(500, 0), None);
        assert_eq!(
            c.plan(503, 0),
            Some(LogPlan {
                from: 501,
                to: 503,
                skipped: 0
            })
        );
    }

    #[test]
    fn log_cursor_chunks_by_max_range() {
        let mut c = LogCursor::new(10);
        c.advance(100);
        let p = c.plan(150, 0).unwrap();
        assert_eq!((p.from, p.to), (101, 110));
        c.advance(p.to);
        let p2 = c.plan(150, 0).unwrap();
        assert_eq!((p2.from, p2.to), (111, 120));
    }

    #[test]
    fn log_cursor_caps_lookback_and_reports_skipped() {
        let mut c = LogCursor::new(1_000_000);
        c.advance(0); // next_from = 1
        let p = c.plan(100_000, 0).unwrap();
        assert_eq!(p.from, 100_000 - LOGS_MAX_LOOKBACK);
        assert_eq!(p.to, 100_000);
        assert_eq!(p.skipped, p.from - 1);
    }

    #[test]
    fn log_cursor_does_not_rewind_when_head_regresses() {
        let mut c = LogCursor::new(1_000);
        c.advance(500); // next_from = 501
        assert_eq!(c.plan(498, 0), None);
    }

    #[test]
    fn backoff_grows_is_bounded_and_resets() {
        let mut b = Backoff::new(1_000, 8_000);
        let d1 = b.next_delay();
        assert!(d1 >= Duration::from_millis(1_000) && d1 <= Duration::from_millis(1_250));
        let d2 = b.next_delay();
        assert!(d2 >= Duration::from_millis(2_000));
        for _ in 0..10 {
            let d = b.next_delay();
            assert!(d <= Duration::from_millis(8_000 + 8_000 / 4));
        }
        b.reset();
        let d = b.next_delay();
        assert!(d <= Duration::from_millis(1_250));
    }
}
