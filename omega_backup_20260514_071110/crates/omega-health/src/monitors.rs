// crates/omega-health/src/monitors.rs
//
// Health monitors â€” background tasks that observe system metrics and
// drive layer health transitions.
//
// Each monitor is a Tokio task.  Monitors do not own layer health
// controllers; they hold `Arc<dyn LayerHealth>` references and call
// `set_state` when thresholds are crossed.
//
// Spec references:
//   Â§3    â€” Health FSM; monitors drive Healthy â†” Degraded â†” Halted
//   Â§11.1 â€” LA tier monitor: hot/warm/cold/archived position counts
//   Â§11.4 â€” Reorg guard integration
//   Â§16   â€” Observability: monitor events are always-sampled
//
// ## Monitors implemented here
//
//   OracleLivenessMonitor  â€” watches oracle heartbeat timestamps.
//     Degraded if any feed is silent for > oracle_stale_threshold_ms.
//     Halted if ALL primary feeds are simultaneously silent.
//
//   GasSpikeMonitor        â€” watches the FeeSnapshot channel.
//     Degraded when base_fee_gwei exceeds the configured spike threshold.
//     Recovered when base_fee drops back below threshold.
//     Used by the Gas War Engine to trigger MissGasSpike drops (Â§7).
//
//   HaltPollLoop           â€” the 10ms polling loop that checks HaltFlag
//     and forwards it to the relay/scoring shutdown channels.
//     SLA: halt visible to all loops within 200ms (Â§3).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::time;

use omega_core::{HealthState, LayerHealth};

use crate::halt::HaltFlag;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OracleLivenessMonitor
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Tracks oracle feed heartbeats and drives ExternalData layer health.
///
/// Each oracle feed is registered with a name and a maximum allowed
/// silence duration.  The monitor polls registered feeds at `poll_interval`
/// and transitions the ExternalData layer:
///
///   - Any feed silent > `stale_threshold` â†’ Degraded
///   - All primary feeds simultaneously silent â†’ Halted
///
/// Spec Â§3: ExternalData Halted cascades to Eil, Strategy, HotPath.
pub struct OracleLivenessMonitor {
    feeds:           Vec<OracleFeedHandle>,
    layer:           Arc<dyn LayerHealth>,
    poll_interval:   Duration,
    stale_threshold: Duration,
}

/// A handle to an oracle feed's liveness state.
///
/// Updated by the oracle layer (omega-oracle) each time a new value is
/// received.  The monitor reads `last_seen_at` without locking by using
/// an atomic timestamp (milliseconds since epoch as `AtomicU64`).
pub struct OracleFeedHandle {
    pub name:       String,
    pub is_primary: bool,
    last_seen_at:   Arc<std::sync::atomic::AtomicU64>,
}

impl OracleFeedHandle {
    /// Create a new feed handle, initialised to `now`.
    pub fn new(name: impl Into<String>, is_primary: bool) -> Arc<Self> {
        let now_ms = Utc::now().timestamp_millis() as u64;
        Arc::new(Self {
            name:         name.into(),
            is_primary,
            last_seen_at: Arc::new(std::sync::atomic::AtomicU64::new(now_ms)),
        })
    }

    /// Record a heartbeat from this feed (called by omega-oracle on every
    /// successful value receipt).
    pub fn heartbeat(&self) {
        let now_ms = Utc::now().timestamp_millis() as u64;
        self.last_seen_at
            .store(now_ms, std::sync::atomic::Ordering::Release);
    }

    /// Milliseconds since the last heartbeat.
    pub fn silence_ms(&self) -> u64 {
        let last = self.last_seen_at.load(std::sync::atomic::Ordering::Acquire);
        let now  = Utc::now().timestamp_millis() as u64;
        now.saturating_sub(last)
    }
}

impl OracleLivenessMonitor {
    pub fn new(
        feeds:           Vec<Arc<OracleFeedHandle>>,
        layer:           Arc<dyn LayerHealth>,
        poll_interval:   Duration,
        stale_threshold: Duration,
    ) -> Self {
        let _ = feeds; // placeholder â€” see with_handles() below
        Self {
            feeds: vec![],
            layer,
            poll_interval,
            stale_threshold,
        }
    }

    /// Convenience builder used in production wiring.
    pub fn with_handles(
        handles:         Vec<Arc<OracleFeedHandle>>,
        layer:           Arc<dyn LayerHealth>,
        poll_interval:   Duration,
        stale_threshold: Duration,
    ) -> (Self, Vec<Arc<OracleFeedHandle>>) {
        let feeds: Vec<OracleFeedHandle> = handles
            .iter()
            .map(|h| OracleFeedHandle {
                name:         h.name.clone(),
                is_primary:   h.is_primary,
                last_seen_at: h.last_seen_at.clone(),
            })
            .collect();

        let monitor = Self { feeds, layer, poll_interval, stale_threshold };
        (monitor, handles)
    }

    /// Run the liveness monitor.  Does not return until `shutdown` fires.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut interval = time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.evaluate();
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("OracleLivenessMonitor shutting down");
                        break;
                    }
                }
            }
        }
    }

    fn evaluate(&self) {
        let threshold_ms      = self.stale_threshold.as_millis() as u64;
        let mut any_stale     = false;
        let mut all_primary_stale = true;
        let mut has_primary   = false;

        for feed in &self.feeds {
            let silence = feed.silence_ms();
            if silence > threshold_ms {
                any_stale = true;
                tracing::warn!(
                    feed       = %feed.name,
                    silence_ms = silence,
                    "Oracle feed stale",
                );
            } else if feed.is_primary {
                all_primary_stale = false;
            }

            if feed.is_primary {
                has_primary = true;
            }
        }

        if !has_primary {
            all_primary_stale = false;
        }

        let target = if all_primary_stale && has_primary {
            HealthState::Halted
        } else if any_stale {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        };

        let current = self.layer.state();
        if current != target {
            let reason = match target {
                HealthState::Halted   => "all primary oracle feeds simultaneously stale",
                HealthState::Degraded => "one or more oracle feeds stale",
                HealthState::Healthy  => "all oracle feeds recovered",
            };
            self.layer.set_state(target, reason);
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// GasSpikeMonitor
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Watches base fee and flags a Risk-layer Degraded when a gas spike is
/// detected (Â§7, Â§12).
pub struct GasSpikeMonitor {
    layer:                Arc<dyn LayerHealth>,
    spike_threshold_gwei: u64,
    current_base_fee:     Arc<std::sync::atomic::AtomicU64>,
    poll_interval:        Duration,
}

impl GasSpikeMonitor {
    pub fn new(
        layer:                Arc<dyn LayerHealth>,
        spike_threshold_gwei: u64,
        current_base_fee:     Arc<std::sync::atomic::AtomicU64>,
        poll_interval:        Duration,
    ) -> Self {
        Self { layer, spike_threshold_gwei, current_base_fee, poll_interval }
    }

    /// Run the gas spike monitor.  Does not return until `shutdown`.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut interval = time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.evaluate();
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("GasSpikeMonitor shutting down");
                        break;
                    }
                }
            }
        }
    }

    pub fn evaluate(&self) {
        let fee_gwei   = self.current_base_fee
            .load(std::sync::atomic::Ordering::Acquire);
        let is_spiking = fee_gwei > self.spike_threshold_gwei;
        let current    = self.layer.state();

        match (current, is_spiking) {
            (HealthState::Healthy, true) => {
                tracing::warn!(
                    base_fee_gwei   = fee_gwei,
                    spike_threshold = self.spike_threshold_gwei,
                    "Gas spike detected â€” transitioning to DEGRADED",
                );
                self.layer.set_state(
                    HealthState::Degraded,
                    &format!(
                        "gas spike: {fee_gwei} gwei > threshold {}",
                        self.spike_threshold_gwei
                    ),
                );
            }
            (HealthState::Degraded, false) => {
                tracing::info!(
                    base_fee_gwei = fee_gwei,
                    "Gas spike resolved â€” recovering to HEALTHY",
                );
                self.layer.set_state(
                    HealthState::Healthy,
                    &format!(
                        "gas spike resolved: {fee_gwei} gwei â‰¤ threshold {}",
                        self.spike_threshold_gwei
                    ),
                );
            }
            // Halted layers are not recovered by the gas monitor â€”
            // recovery requires governance clearance (Â§5).
            _ => {}
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// HaltPollLoop
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// 10ms halt polling loop â€” bridges HaltFlag to a `watch` channel.
///
/// SLA: halt visible to all loops within 200ms (Â§3).
pub struct HaltPollLoop {
    flag:    HaltFlag,
    halt_tx: tokio::sync::watch::Sender<bool>,
}

impl HaltPollLoop {
    /// Create a new poll loop and return (loop, halt_receiver).
    pub fn new(flag: HaltFlag) -> (Self, tokio::sync::watch::Receiver<bool>) {
        let (tx, rx) = tokio::sync::watch::channel(flag.is_halted());
        (Self { flag, halt_tx: tx }, rx)
    }

    /// Run the poll loop.  Does not return until `shutdown`.
    pub async fn run(self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        const POLL_MS: u64 = 10;
        let mut interval = time::interval(Duration::from_millis(POLL_MS));
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        let mut last_state = self.flag.is_halted();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let current = self.flag.is_halted();
                    if current != last_state {
                        last_state = current;
                        let _ = self.halt_tx.send(current);
                        if current {
                            tracing::error!(
                                "HaltPollLoop: halt flag set â€” notifying {} receiver(s)",
                                self.halt_tx.receiver_count(),
                            );
                        } else {
                            tracing::warn!("HaltPollLoop: halt flag cleared");
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("HaltPollLoop shutting down");
                        break;
                    }
                }
            }
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use omega_core::LayerId;
    use crate::state_machine::LayerHealthImpl;

    fn make_layer(id: LayerId) -> Arc<LayerHealthImpl> {
        LayerHealthImpl::new_bare(id)
    }

    // â”€â”€ OracleFeedHandle â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn feed_heartbeat_resets_silence() {
        let feed = OracleFeedHandle::new("chainlink_eth", true);
        feed.last_seen_at.store(0, Ordering::SeqCst); // far in the past
        assert!(feed.silence_ms() > 0);
        feed.heartbeat();
        assert!(feed.silence_ms() < 100, "heartbeat should reset silence to near-zero");
    }

    // â”€â”€ GasSpikeMonitor â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn gas_spike_monitor_transitions_to_degraded() {
        let layer   = make_layer(LayerId::Risk);
        let fee     = Arc::new(AtomicU64::new(50));
        let monitor = GasSpikeMonitor::new(
            layer.clone(),
            100,
            fee.clone(),
            Duration::from_millis(10),
        );

        monitor.evaluate();
        assert_eq!(layer.state(), HealthState::Healthy);

        fee.store(150, Ordering::SeqCst);
        monitor.evaluate();
        assert_eq!(layer.state(), HealthState::Degraded);

        fee.store(80, Ordering::SeqCst);
        monitor.evaluate();
        assert_eq!(layer.state(), HealthState::Healthy);
    }

    #[test]
    fn gas_spike_monitor_does_not_recover_halted_layer() {
        let layer   = make_layer(LayerId::Risk);
        let fee     = Arc::new(AtomicU64::new(50));
        let monitor = GasSpikeMonitor::new(
            layer.clone(),
            100,
            fee.clone(),
            Duration::from_millis(10),
        );

        layer.set_state(HealthState::Halted, "independent fault");
        fee.store(20, Ordering::SeqCst);
        monitor.evaluate();
        assert_eq!(
            layer.state(),
            HealthState::Halted,
            "Halted layer must not be recovered by gas spike monitor"
        );
    }

    // â”€â”€ HaltPollLoop â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[tokio::test]
    async fn halt_poll_loop_propagates_halt() {
        let flag                   = HaltFlag::new();
        let (loop_, mut rx)        = HaltPollLoop::new(flag.clone());
        let (shutdown_tx, shut_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(loop_.run(shut_rx));

        flag.halt(LayerId::SystemHealth, "test");
        tokio::time::timeout(Duration::from_millis(200), rx.changed())
            .await
            .expect("halt must propagate within 200ms")
            .expect("channel not closed");

        assert!(*rx.borrow(), "halt receiver must be true");
        let _ = shutdown_tx.send(true);
    }
}