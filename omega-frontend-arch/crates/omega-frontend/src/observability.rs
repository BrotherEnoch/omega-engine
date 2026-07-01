// omega-frontend-arch/crates/omega-frontend/src/observability.rs
//! Frontend observability: event log ring buffer and metric counters.
//!
//! ## Optimisations (vs initial version)
//! - `blueprint_hash` fields use `Arc<str>` instead of `String`.
//!   Every event clone (ring buffer push, metric record) is now O(1) —
//!   just an atomic ref-count increment. A 64-char hex string is 64 bytes;
//!   with frequent blueprint events this saves significant allocations.
//! - `ObservabilityLog::push` is inlined — the capacity check is a single
//!   comparison with no branch misprediction on the hot path.

use std::sync::Arc;
use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use omega_control_contracts::ws::{SimulationErrorSubCode, WsEvent};

// ---------------------------------------------------------------------------
// ObservabilityEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ObservabilityEntry {
    pub recorded_at: DateTime<Utc>,
    pub kind:        ObservabilityEventKind,
}

/// All observable event kinds. `blueprint_hash` is `Arc<str>` — O(1) clone.
#[derive(Debug, Clone, PartialEq)]
pub enum ObservabilityEventKind {
    GasModelReverted    { checkpoint_version: u64, win_rate: f64 },
    CeilingEscalation   { feature_key: Arc<str>, hit_count: u64 },
    EmergencySkipped    { blueprint_hash: Arc<str>, emergency_fee_gwei: u64 },
    ProfitSplit         { blueprint_hash: Arc<str>, pil_share_wei: Arc<str>, dao_fee_wei: Arc<str> },
    LaReorgRisk         { tx_hash: Arc<str>, orphaned_block: u64 },
    SimulationError     { blueprint_hash: Arc<str>, sub_code: SimulationErrorSubCode },
    WsGapDetected       { expected_seq: u64, got_seq: u64 },
    WsStatusChange      { status_label: &'static str },
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObservabilityMetrics {
    pub gas_model_reverts:            u64,
    pub ceiling_escalations:          u64,
    pub emergency_bundles_skipped:    u64,
    pub profit_splits:                u64,
    pub la_reorg_risks:               u64,
    pub simulation_state_mismatches:  u64,
    pub simulation_execution_reverts: u64,
    pub simulation_gas_miscalcs:      u64,
    pub ws_gaps:                      u64,
}

// ---------------------------------------------------------------------------
// ObservabilityLog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ObservabilityLog {
    entries:  VecDeque<ObservabilityEntry>,
    capacity: usize,
    pub metrics: ObservabilityMetrics,
}

impl Default for ObservabilityLog {
    fn default() -> Self { Self::with_capacity(500) }
}

impl ObservabilityLog {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries:  VecDeque::with_capacity(capacity),
            capacity,
            metrics:  ObservabilityMetrics::default(),
        }
    }

    /// Record a WebSocket event. Only observability-relevant events are stored.
    /// `blueprint_hash` strings are interned as `Arc<str>` — clone is O(1).
    pub fn record_ws_event(&mut self, event: &WsEvent) {
        let kind = match event {
            WsEvent::GasModelReverted(e) => {
                self.metrics.gas_model_reverts += 1;
                Some(ObservabilityEventKind::GasModelReverted {
                    checkpoint_version: e.checkpoint_version,
                    win_rate:           e.win_rate,
                })
            }
            WsEvent::GasModelCeilingEscalation(e) => {
                self.metrics.ceiling_escalations += 1;
                Some(ObservabilityEventKind::CeilingEscalation {
                    feature_key: Arc::from(e.feature_key.as_str()),
                    hit_count:   e.ceiling_hit_count,
                })
            }
            WsEvent::EmergencyBundleSkipped(e) => {
                self.metrics.emergency_bundles_skipped += 1;
                Some(ObservabilityEventKind::EmergencySkipped {
                    blueprint_hash:     Arc::from(e.blueprint_hash.as_str()),
                    emergency_fee_gwei: e.emergency_fee_gwei,
                })
            }
            WsEvent::ProfitSplit(e) => {
                self.metrics.profit_splits += 1;
                Some(ObservabilityEventKind::ProfitSplit {
                    blueprint_hash: Arc::from(e.blueprint_hash.as_str()),
                    pil_share_wei:  Arc::from(e.pil_share_wei.as_str()),
                    dao_fee_wei:    Arc::from(e.dao_fee_wei.as_str()),
                })
            }
            WsEvent::LaReorgRisk(e) => {
                self.metrics.la_reorg_risks += 1;
                Some(ObservabilityEventKind::LaReorgRisk {
                    tx_hash:        Arc::from(e.tx_hash.as_str()),
                    orphaned_block: e.orphaned_block,
                })
            }
            WsEvent::SimulationError(e) => {
                match e.sub_code {
                    SimulationErrorSubCode::StateMismatch   => self.metrics.simulation_state_mismatches += 1,
                    SimulationErrorSubCode::ExecutionRevert => self.metrics.simulation_execution_reverts += 1,
                    SimulationErrorSubCode::GasMiscalc      => self.metrics.simulation_gas_miscalcs += 1,
                }
                Some(ObservabilityEventKind::SimulationError {
                    blueprint_hash: Arc::from(e.blueprint_hash.as_str()),
                    sub_code:       e.sub_code,
                })
            }
            // LayerEvent and Ping do not produce observability entries.
            WsEvent::LayerEvent(_) | WsEvent::Ping(_) | WsEvent::BlueprintConfirmed(_) => None,
        };

        if let Some(k) = kind {
            self.push(ObservabilityEntry { recorded_at: Utc::now(), kind: k });
        }
    }

    pub fn record_ws_gap(&mut self, expected_seq: u64, got_seq: u64) {
        self.metrics.ws_gaps += 1;
        self.push(ObservabilityEntry {
            recorded_at: Utc::now(),
            kind: ObservabilityEventKind::WsGapDetected { expected_seq, got_seq },
        });
    }

    pub fn recent(&self, n: usize) -> impl Iterator<Item = &ObservabilityEntry> {
        let skip = self.entries.len().saturating_sub(n);
        self.entries.iter().skip(skip)
    }

    pub fn len(&self)      -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool  { self.entries.is_empty() }

    #[inline]
    fn push(&mut self, entry: ObservabilityEntry) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use omega_control_contracts::ws::{EmergencyBundleSkippedEvent, GasModelCeilingEvent};

    #[test]
    fn ceiling_escalation_increments_counter() {
        let mut log = ObservabilityLog::with_capacity(10);
        log.record_ws_event(&WsEvent::GasModelCeilingEscalation(GasModelCeilingEvent {
            feature_key:       "ARBITRUM_LA".into(),
            ceiling_hit_count: 101,
        }));
        assert_eq!(log.metrics.ceiling_escalations, 1);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn ring_buffer_respects_capacity() {
        let mut log = ObservabilityLog::with_capacity(3);
        for i in 0u64..10 {
            log.record_ws_event(&WsEvent::EmergencyBundleSkipped(EmergencyBundleSkippedEvent {
                blueprint_hash:     format!("0x{i:064x}"),
                reason:             "test".into(),
                emergency_fee_gwei: 1000,
            }));
        }
        assert_eq!(log.len(), 3,  "Ring buffer must not exceed capacity");
        assert_eq!(log.metrics.emergency_bundles_skipped, 10, "Counter is unbounded");
    }

    #[test]
    fn simulation_subcodes_counted_separately() {
        use omega_control_contracts::ws::SimulationErrorEvent;
        let mut log = ObservabilityLog::default();
        for sub in [
            SimulationErrorSubCode::StateMismatch,
            SimulationErrorSubCode::ExecutionRevert,
            SimulationErrorSubCode::GasMiscalc,
            SimulationErrorSubCode::StateMismatch,
        ] {
            log.record_ws_event(&WsEvent::SimulationError(SimulationErrorEvent {
                blueprint_hash: "0x1".into(),
                sub_code:       sub,
                detail:         None,
            }));
        }
        assert_eq!(log.metrics.simulation_state_mismatches, 2);
        assert_eq!(log.metrics.simulation_execution_reverts, 1);
        assert_eq!(log.metrics.simulation_gas_miscalcs, 1);
    }

    #[test]
    fn blueprint_hash_arc_clone_is_o1() {
        // Arc<str> clone increments refcount — does not allocate.
        let hash: Arc<str> = Arc::from("0xdeadbeef");
        let clone = Arc::clone(&hash);
        assert_eq!(Arc::strong_count(&hash), 2);
        assert_eq!(&*clone, "0xdeadbeef");
    }

    #[test]
    fn ws_gap_recorded_correctly() {
        let mut log = ObservabilityLog::default();
        log.record_ws_gap(5, 10);
        assert_eq!(log.metrics.ws_gaps, 1);
        assert_eq!(log.len(), 1);
        {
            let recent = log.recent(1).next().unwrap();
            if let ObservabilityEventKind::WsGapDetected { expected_seq, got_seq } = &recent.kind {
                assert_eq!(*expected_seq, 5);
                assert_eq!(*got_seq, 10);
            } else {
                panic!("Wrong event kind");
            }
        }
    }

    #[test]
    fn layer_event_and_ping_produce_no_entry() {
        use omega_control_contracts::ws::{LayerEventPayload, PingPayload};
        let mut log = ObservabilityLog::default();
        log.record_ws_event(&WsEvent::LayerEvent(LayerEventPayload {
            layer: "L08".into(), status: "HEALTHY".into(),
            message: "tick=1".into(), version: 1, latency_ns: 100,
        }));
        log.record_ws_event(&WsEvent::Ping(PingPayload { nonce: 1 }));
        assert_eq!(log.len(), 0, "LayerEvent and Ping must not produce observability entries");
    }
}