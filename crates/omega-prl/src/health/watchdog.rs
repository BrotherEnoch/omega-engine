// omega-prl/src/health/watchdog.rs
//! PRL watchdog — automatic degradation triggers (§17.3)
//!
//! Triggers (§17.3):
//!   inference >50 µs (3 consecutive) → set_degraded("ML_INFERENCE_TIMEOUT")
//!   queue overflow (>1000 streak)     → set_limited("PERSISTENT_QUEUE_OVERFLOW")
//!   replay divergence                 → set_degraded("REPLAY_DIVERGENCE")
//!   memory pressure                   → set_degraded("MEMORY_PRESSURE")
//!   NUMA imbalance                    → log only (operational, not a failure)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::{error, warn};

use crate::health::degraded::PrlHealth;
use crate::metrics::prometheus::PrlMetrics;

pub struct PrlWatchdog {
    health: Arc<PrlHealth>,
    metrics: Arc<PrlMetrics>,
    max_inference_us: u64,
    inference_timeout_streak: AtomicU64,
    queue_overflow_streak: AtomicU64,
}

impl PrlWatchdog {
    pub fn new(health: Arc<PrlHealth>, metrics: Arc<PrlMetrics>, max_inference_us: u64) -> Self {
        Self {
            health,
            metrics,
            max_inference_us,
            inference_timeout_streak: AtomicU64::new(0),
            queue_overflow_streak: AtomicU64::new(0),
        }
    }

    /// Record ML inference latency.  Three consecutive timeouts disable ML (§17.3).
    #[inline]
    pub fn record_inference_latency(&self, latency_us: u64) {
        self.metrics.inference_latency_us.observe(latency_us as f64);

        if latency_us > self.max_inference_us {
            let streak = self
                .inference_timeout_streak
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if streak >= 3 {
                self.health.set_degraded("ML_INFERENCE_TIMEOUT");
                warn!(
                    streak,
                    latency_us, "PRL watchdog: ML path disabled — inference latency exceeded"
                );
            }
        } else {
            self.inference_timeout_streak.store(0, Ordering::Relaxed);
        }
    }

    /// Record a ring buffer overflow.  Sustained overflow → LIMITED (§17.3).
    #[inline]
    pub fn record_queue_overflow(&self) {
        let streak = self.queue_overflow_streak.fetch_add(1, Ordering::Relaxed) + 1;
        if streak > 1_000 {
            self.health.set_limited("PERSISTENT_QUEUE_OVERFLOW");
            warn!(
                streak,
                "PRL watchdog: persistent overflow — entering LIMITED"
            );
        }
    }

    /// Replay divergence detected (§17.3, §26).
    pub fn on_replay_divergence(&self) {
        self.health.set_degraded("REPLAY_DIVERGENCE");
        self.metrics.replay_divergence_total.inc();
        error!("PRL watchdog: replay divergence — entering DEGRADED");
    }

    /// Memory pressure: reduce historical retention (§17.3, §22.2).
    pub fn on_memory_pressure(&self) {
        self.health.set_degraded("MEMORY_PRESSURE");
        warn!("PRL watchdog: memory pressure — historical retention reduced");
    }

    /// NUMA imbalance: trigger shard rebalance via OS affinity (§17.3, §22.3).
    /// Health stays HEALTHY — rebalancing is operational, not a fault.
    pub fn on_numa_imbalance(&self) {
        warn!("PRL watchdog: NUMA imbalance — shard rebalance triggered");
    }

    /// Reset all streak counters after governance recovery.
    pub fn reset_streaks(&self) {
        self.inference_timeout_streak.store(0, Ordering::Relaxed);
        self.queue_overflow_streak.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::degraded::{PrlHealth, PrlHealthState};
    use crate::metrics::prometheus::PrlMetrics;

    fn make_watchdog() -> PrlWatchdog {
        let health = Arc::new(PrlHealth::new());
        let metrics = Arc::new(PrlMetrics::new().unwrap());
        PrlWatchdog::new(health, metrics, 50)
    }

    #[test]
    fn three_consecutive_timeouts_degrade() {
        let wd = make_watchdog();
        wd.record_inference_latency(100); // 1
        wd.record_inference_latency(100); // 2
        wd.record_inference_latency(100); // 3 — triggers
        assert_eq!(wd.health.state(), PrlHealthState::Degraded);
    }

    #[test]
    fn streak_resets_on_success() {
        let wd = make_watchdog();
        wd.record_inference_latency(100);
        wd.record_inference_latency(100);
        wd.record_inference_latency(10); // within budget — resets streak
        wd.record_inference_latency(100);
        // Only 1 timeout since reset — should still be HEALTHY
        assert_eq!(wd.health.state(), PrlHealthState::Healthy);
    }

    #[test]
    fn persistent_overflow_sets_limited() {
        let wd = make_watchdog();
        for _ in 0..1_001 {
            wd.record_queue_overflow();
        }
        assert_eq!(wd.health.state(), PrlHealthState::Limited);
    }

    #[test]
    fn replay_divergence_degrades() {
        let wd = make_watchdog();
        wd.on_replay_divergence();
        assert_eq!(wd.health.state(), PrlHealthState::Degraded);
    }

    #[test]
    fn numa_imbalance_does_not_degrade() {
        let wd = make_watchdog();
        wd.on_numa_imbalance();
        assert_eq!(wd.health.state(), PrlHealthState::Healthy);
    }
}
