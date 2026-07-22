// crates/omega-loss-attribution/src/dashboard.rs
//
// Attribution pipeline observability dashboard (spec Â§16, Â§17.2).
//
// ## Spec Â§16
//
//   LA events are always-sampled (100% sampling rate).  The dashboard
//   aggregates all events for the Prometheus metrics scrape target and
//   the `GET /api/v1/la/gas-model/checkpoints` API endpoint.
//
// ## Spec Â§17.2
//
//   New API endpoints added in v12 include:
//     GET /api/v1/la/gas-model/checkpoints  â€” checkpoint listing
//     GET /api/v1/la/gas-model/ceiling-status â€” escalation state
//
//   The dashboard serves as the in-process aggregator that backs these
//   endpoints.  It is NOT async â€” snapshots are taken by the control-
//   plane on demand and serialised there.
//
// ## Design corrections
//
//   The submitted code used `crate::validation::LossEvent` for the
//   dashboard, but that type was removed in favour of `PipelineLossEvent`
//   (which carries `estimated_loss_usd`).  `EscalationReason` was also
//   removed â€” ceiling escalation tracking is now in `ceiling_escalation.rs`
//   and uses `CeilingEscalationState`.
//
//   `timestamp_ms` in the snapshot was relative to the dashboard start
//   time (`Instant::elapsed().as_millis()`), making it useless for
//   external consumers who need a wall-clock timestamp.  Replaced with
//   `chrono::Utc::now()`.
//
//   `health_score` formula `(success_rate - rejection_penalty)` is
//   wrong: for any non-zero rejection count it can produce negative
//   values even at a 99% success rate (e.g. 99 valid / 1 rejected â†’
//   score = 0.99 - 0.01 = 0.98, which is fine, but 50/50 â†’ 0.0 instead
//   of the expected 0.5).  The correct formula is simply the success
//   rate: `valid / total`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::ceiling_escalation::CeilingEscalationState;
use super::classifier::LossCode;
use super::validation::PipelineLossEvent;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// DashboardSnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Immutable point-in-time snapshot of pipeline attribution state.
///
/// Serialised to JSON by the control-plane for the API response.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardSnapshot {
    /// UTC timestamp of this snapshot.
    pub timestamp: DateTime<Utc>,

    /// Total pipeline events (valid + rejected).
    pub total_events: u64,

    /// Events that passed validation and entered attribution.
    pub valid_events: u64,

    /// Events that failed validation (see `rejection_breakdown`).
    pub rejected_events: u64,

    /// Aggregated estimated USD loss across all valid events.
    pub total_loss_usd: f64,

    /// Fraction of events that were valid [0.0, 1.0].
    /// 1.0 when `total_events == 0` (no data is not a failure).
    pub success_rate: f64,

    /// Count of valid events grouped by `LossCode`.
    pub loss_code_counts: HashMap<String, u64>,

    /// Count of rejected events grouped by validation error code.
    pub rejection_breakdown: HashMap<String, u64>,

    /// Current ceiling escalation state (Â§13.3, Â§17.2).
    pub ceiling_state: Option<CeilingEscalationState>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// AttributionDashboard
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// In-process aggregator for the attribution pipeline observability layer.
///
/// Thread safety: `AttributionDashboard` is NOT `Send + Sync` â€” it is
/// intended to be owned by a single task.  If concurrent access is
/// needed, wrap in `Mutex<AttributionDashboard>`.
#[derive(Debug)]
pub struct AttributionDashboard {
    total_events:        u64,
    valid_events:        u64,
    rejected_events:     u64,
    total_loss_usd:      f64,
    loss_code_counts:    HashMap<String, u64>,
    rejection_breakdown: HashMap<String, u64>,
    last_ceiling_state:  Option<CeilingEscalationState>,
}

impl AttributionDashboard {
    /// Create a new, zeroed dashboard.
    pub fn new() -> Self {
        Self {
            total_events:        0,
            valid_events:        0,
            rejected_events:     0,
            total_loss_usd:      0.0,
            loss_code_counts:    HashMap::new(),
            rejection_breakdown: HashMap::new(),
            last_ceiling_state:  None,
        }
    }

    // â”€â”€ Recording â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Record a successfully validated loss event.
    pub fn record_valid_event(&mut self, event: &PipelineLossEvent, code: LossCode) {
        self.total_events  += 1;
        self.valid_events  += 1;
        self.total_loss_usd += event.estimated_loss_usd;

        *self.loss_code_counts
            .entry(code.to_string())
            .or_insert(0) += 1;
    }

    /// Record a validation rejection.
    ///
    /// `error_code` is the `ValidationError::code` (SCREAMING_SNAKE_CASE).
    pub fn record_rejected_event(&mut self, error_code: &'static str) {
        self.total_events    += 1;
        self.rejected_events += 1;

        *self.rejection_breakdown
            .entry(error_code.to_owned())
            .or_insert(0) += 1;
    }

    /// Update the ceiling escalation state (Â§13.3, Â§17.2).
    pub fn update_ceiling_state(&mut self, state: CeilingEscalationState) {
        self.last_ceiling_state = Some(state);
    }

    // â”€â”€ Snapshot â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Generate a full dashboard snapshot.
    ///
    /// Cheap to call â€” all fields are computed from cached counters.
    pub fn snapshot(&self) -> DashboardSnapshot {
        let success_rate = if self.total_events == 0 {
            1.0
        } else {
            self.valid_events as f64 / self.total_events as f64
        };

        DashboardSnapshot {
            timestamp:           Utc::now(),
            total_events:        self.total_events,
            valid_events:        self.valid_events,
            rejected_events:     self.rejected_events,
            total_loss_usd:      self.total_loss_usd,
            success_rate,
            loss_code_counts:    self.loss_code_counts.clone(),
            rejection_breakdown: self.rejection_breakdown.clone(),
            ceiling_state:       self.last_ceiling_state.clone(),
        }
    }

    // â”€â”€ Derived metrics â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Fraction of events that were valid [0.0, 1.0].
    ///
    /// Returns 1.0 when no events have been processed (empty pipeline is
    /// considered healthy, not unknown).
    pub fn success_rate(&self) -> f64 {
        if self.total_events == 0 {
            return 1.0;
        }
        self.valid_events as f64 / self.total_events as f64
    }

    /// Returns `true` when the pipeline is healthy enough for production.
    ///
    /// Threshold: success rate > 0.75 (Â§16 health criteria).
    pub fn is_healthy(&self) -> bool {
        self.success_rate() > 0.75
    }

    /// Human-readable one-line summary for logs and CLI output.
    pub fn summary(&self) -> String {
        format!(
            "Events: {} | Valid: {} | Rejected: {} | Loss: {:.2} USD | \
             SuccessRate: {:.3} | Healthy: {}",
            self.total_events,
            self.valid_events,
            self.rejected_events,
            self.total_loss_usd,
            self.success_rate(),
            self.is_healthy(),
        )
    }

    /// Reset all counters (useful for backtest / simulation run boundaries).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for AttributionDashboard {
    fn default() -> Self {
        Self::new()
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::TxHash;

    fn event(loss_usd: f64) -> PipelineLossEvent {
        PipelineLossEvent {
            tx_hash:            TxHash::from([1u8; 32]),
            estimated_loss_usd: loss_usd,
            cause:              "LOST_GAS_LOW".into(),
        }
    }

    // â”€â”€ Empty state â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn empty_dashboard_is_healthy() {
        let d = AttributionDashboard::new();
        assert!(d.is_healthy());
        assert!((d.success_rate() - 1.0).abs() < 1e-9);
    }

    // â”€â”€ Valid event recording â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn record_valid_event_increments_counters() {
        let mut d = AttributionDashboard::new();
        d.record_valid_event(&event(500.0), LossCode::LostGasLow);
        d.record_valid_event(&event(200.0), LossCode::LostLatency);

        assert_eq!(d.total_events,   2);
        assert_eq!(d.valid_events,   2);
        assert_eq!(d.rejected_events, 0);
        assert!((d.total_loss_usd - 700.0).abs() < 1e-9);
    }

    #[test]
    fn loss_code_counts_populated() {
        let mut d = AttributionDashboard::new();
        d.record_valid_event(&event(100.0), LossCode::LostGasLow);
        d.record_valid_event(&event(100.0), LossCode::LostGasLow);
        d.record_valid_event(&event(100.0), LossCode::LostLatency);

        let snap = d.snapshot();
        assert_eq!(snap.loss_code_counts["LOST_GAS_LOW"],  2);
        assert_eq!(snap.loss_code_counts["LOST_LATENCY"],  1);
    }

    // â”€â”€ Rejected event recording â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn record_rejected_event_increments_counters() {
        let mut d = AttributionDashboard::new();
        d.record_rejected_event("ZERO_TX_HASH");
        d.record_rejected_event("ZERO_TX_HASH");
        d.record_rejected_event("INVALID_BLOCK");

        assert_eq!(d.total_events,    3);
        assert_eq!(d.rejected_events, 3);
        assert_eq!(d.valid_events,    0);

        let snap = d.snapshot();
        assert_eq!(snap.rejection_breakdown["ZERO_TX_HASH"],  2);
        assert_eq!(snap.rejection_breakdown["INVALID_BLOCK"],  1);
    }

    // â”€â”€ Success rate â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn success_rate_correct() {
        let mut d = AttributionDashboard::new();
        for _ in 0..75 { d.record_valid_event(&event(1.0), LossCode::LostGasLow); }
        for _ in 0..25 { d.record_rejected_event("NAN_LOSS"); }

        assert!((d.success_rate() - 0.75).abs() < 1e-9);
        // At exactly 0.75 the pipeline is NOT healthy (threshold is >0.75)
        assert!(!d.is_healthy());
    }

    #[test]
    fn success_rate_76_pct_is_healthy() {
        let mut d = AttributionDashboard::new();
        for _ in 0..76 { d.record_valid_event(&event(1.0), LossCode::LostGasLow); }
        for _ in 0..24 { d.record_rejected_event("NAN_LOSS"); }
        assert!(d.is_healthy());
    }

    // â”€â”€ Reset â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn reset_clears_all_state() {
        let mut d = AttributionDashboard::new();
        d.record_valid_event(&event(999.0), LossCode::LostGasLow);
        d.record_rejected_event("ZERO_TX_HASH");
        d.reset();

        assert_eq!(d.total_events,   0);
        assert_eq!(d.valid_events,   0);
        assert_eq!(d.rejected_events, 0);
        assert!((d.total_loss_usd - 0.0).abs() < 1e-9);
        assert!(d.snapshot().loss_code_counts.is_empty());
    }

    // â”€â”€ Snapshot â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn snapshot_success_rate_matches_computed() {
        let mut d = AttributionDashboard::new();
        for _ in 0..80 { d.record_valid_event(&event(1.0), LossCode::LostGasLow); }
        for _ in 0..20 { d.record_rejected_event("INVALID_BLOCK"); }

        let snap = d.snapshot();
        assert!((snap.success_rate - 0.80).abs() < 1e-9);
    }
}