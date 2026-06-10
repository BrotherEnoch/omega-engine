// crates/omega-loss-attribution/src/ceiling_escalation.rs
//
// Gas model ceiling escalation — spec §13.3 (fix I5).
//
// ## What this module is
//
// "Ceiling escalation" in v12 refers specifically to the behaviour when
// the ML fee multiplier reaches its 5.0× upper bound and the model
// continues to receive `LostGasLow` events — meaning the engine is
// losing gas wars even when bidding at the maximum configured multiplier.
//
// The spec (§13.3) defines the following response:
//   1. Count consecutive `LostGasLow` events at the 5.0× ceiling.
//   2. If the count exceeds `ceiling_escalation_threshold` (default 100):
//      a. Emit `GAS_MODEL_CEILING_ESCALATION` event.
//      b. Transition the LossAttribution layer to DEGRADED.
//      c. Pause the model — no further multiplier updates until governance
//         clears via POST /api/v1/la/gas-model/unpause (L2 fast-approve).
//
// ## What this module is NOT
//
// The original submitted code implemented a "dynamic USD loss ceiling"
// with stress metrics (gas volatility, mempool pressure, oracle
// instability) that does not appear anywhere in the v12 spec.  That
// logic has been removed entirely.
//
// ## CeilingEscalationState
//
// This module provides a small, observable state struct that the online
// learner (`online_learner.rs`) updates and the observability layer
// (§16) samples for the `GET /api/v1/la/gas-model/ceiling-status` API
// endpoint (§17.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::classifier::FeatureKey;

// ─────────────────────────────────────────────────────────────────────────────
// CeilingEscalationState
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot of the ceiling escalation state for the observability API
/// (`GET /api/v1/la/gas-model/ceiling-status`, §17.2).
///
/// Produced by `CeilingEscalationTracker::snapshot()` and serialised
/// to JSON by the control-plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CeilingEscalationState {
    /// Whether the model is currently paused due to ceiling escalation.
    pub paused: bool,

    /// Number of consecutive `LostGasLow` events at the 5.0× ceiling
    /// across all feature keys combined.
    pub consecutive_ceiling_hits: u64,

    /// The configured threshold before escalation triggers.
    pub escalation_threshold: u64,

    /// Feature key that triggered the most recent ceiling hit (if any).
    pub trigger_key: Option<String>,

    /// UTC timestamp of the most recent ceiling hit.
    pub last_hit_at: Option<DateTime<Utc>>,

    /// UTC timestamp when the model was paused (if currently paused).
    pub paused_at: Option<DateTime<Utc>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// CeilingEscalationTracker
// ─────────────────────────────────────────────────────────────────────────────

/// Tracks ceiling escalation state for the observability API.
///
/// The online learner (`GasModelOnlineLearner`) owns this tracker and
/// updates it on every `LostGasLow` at the ceiling.  The control-plane
/// reads snapshots via `snapshot()`.
///
/// This struct is intentionally NOT responsible for pausing the model —
/// that logic lives in `GasModelOnlineLearner::update_multiplier` where
/// it has direct access to the atomics.  This struct is purely for
/// observability.
#[derive(Debug)]
pub struct CeilingEscalationTracker {
    threshold: u64,
    consecutive_hits: u64,
    trigger_key: Option<FeatureKey>,
    last_hit_at: Option<DateTime<Utc>>,
    paused_at: Option<DateTime<Utc>>,
}

impl CeilingEscalationTracker {
    /// Create a tracker with the given escalation threshold.
    ///
    /// `threshold` should be `MlConfig::ceiling_escalation_threshold`
    /// (default 100).
    pub fn new(threshold: u64) -> Self {
        Self {
            threshold,
            consecutive_hits: 0,
            trigger_key: None,
            last_hit_at: None,
            paused_at: None,
        }
    }

    /// Record one `LostGasLow` event at the multiplier ceiling.
    ///
    /// Returns `true` when the hit count has crossed `threshold` — the
    /// caller should then pause the model and emit the DEGRADED health
    /// transition.
    pub fn record_ceiling_hit(&mut self, key: &FeatureKey) -> bool {
        self.consecutive_hits += 1;
        self.trigger_key = Some(key.clone());
        self.last_hit_at = Some(Utc::now());

        let crossed = self.consecutive_hits > self.threshold;

        tracing::debug!(
            feature_key       = %key.label(),
            consecutive_hits  = self.consecutive_hits,
            threshold         = self.threshold,
            threshold_crossed = crossed,
            "Ceiling hit recorded",
        );

        crossed
    }

    /// Record that the model has been paused.
    pub fn record_pause(&mut self) {
        self.paused_at = Some(Utc::now());
        tracing::warn!(
            consecutive_hits = self.consecutive_hits,
            threshold = self.threshold,
            trigger_key = self.trigger_key.as_ref().map(|k| k.label()),
            "GAS_MODEL_CEILING_ESCALATION: model paused pending L2 governance",
        );
    }

    /// Reset the consecutive hit counter.
    ///
    /// Called by the online learner when a non-ceiling update occurs
    /// (i.e. the multiplier moved away from the ceiling).
    pub fn reset_hits(&mut self) {
        self.consecutive_hits = 0;
        self.trigger_key = None;
    }

    /// Record that the model has been unpaused by governance.
    pub fn record_unpause(&mut self) {
        self.paused_at = None;
        self.consecutive_hits = 0;
        self.trigger_key = None;
        tracing::info!("GAS_MODEL_CEILING_ESCALATION: model unpaused by governance");
    }

    /// Snapshot of current escalation state for the API (§17.2).
    pub fn snapshot(&self, paused: bool) -> CeilingEscalationState {
        CeilingEscalationState {
            paused,
            consecutive_ceiling_hits: self.consecutive_hits,
            escalation_threshold: self.threshold,
            trigger_key: self.trigger_key.as_ref().map(|k| k.label()),
            last_hit_at: self.last_hit_at,
            paused_at: self.paused_at,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> FeatureKey {
        super::super::classifier::FeatureKey {
            asset_tier: 0,
            hf_urgency: 1,
            protocol: "aave_v3".into(),
            size_tier: 1,
        }
    }

    #[test]
    fn threshold_not_crossed_before_100_hits() {
        let mut t = CeilingEscalationTracker::new(100);
        for _ in 0..100 {
            let crossed = t.record_ceiling_hit(&key());
            // The 100th hit is consecutive_hits = 100, and 100 > 100 is false
            assert!(!crossed, "threshold of 100 requires >100 hits to trigger");
        }
    }

    #[test]
    fn threshold_crossed_at_101_hits() {
        let mut t = CeilingEscalationTracker::new(100);
        let mut crossed = false;
        for _ in 0..=100 {
            crossed = t.record_ceiling_hit(&key());
        }
        assert!(crossed, "101st hit must cross the threshold");
    }

    #[test]
    fn reset_clears_consecutive_hits() {
        let mut t = CeilingEscalationTracker::new(100);
        for _ in 0..50 {
            t.record_ceiling_hit(&key());
        }
        assert_eq!(t.consecutive_hits, 50);
        t.reset_hits();
        assert_eq!(t.consecutive_hits, 0);
        assert!(t.trigger_key.is_none());
    }

    #[test]
    fn snapshot_reflects_state() {
        let mut t = CeilingEscalationTracker::new(100);
        t.record_ceiling_hit(&key());
        let snap = t.snapshot(false);
        assert_eq!(snap.consecutive_ceiling_hits, 1);
        assert_eq!(snap.escalation_threshold, 100);
        assert!(!snap.paused);
        assert!(snap.trigger_key.is_some());
        assert!(snap.last_hit_at.is_some());
    }

    #[test]
    fn record_pause_sets_paused_at() {
        let mut t = CeilingEscalationTracker::new(100);
        t.record_ceiling_hit(&key());
        t.record_pause();
        let snap = t.snapshot(true);
        assert!(snap.paused_at.is_some());
    }

    #[test]
    fn record_unpause_resets_all() {
        let mut t = CeilingEscalationTracker::new(100);
        for _ in 0..50 {
            t.record_ceiling_hit(&key());
        }
        t.record_pause();
        t.record_unpause();
        let snap = t.snapshot(false);
        assert_eq!(snap.consecutive_ceiling_hits, 0);
        assert!(snap.trigger_key.is_none());
        assert!(snap.paused_at.is_none());
    }
}
