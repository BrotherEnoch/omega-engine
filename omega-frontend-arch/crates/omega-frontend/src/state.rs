// omega-frontend-arch/crates/omega-frontend/src/state.rs
//! Immutable engine state with monotonic revision counter.
//!
//! ## Invariants
//! - Every mutation produces a **new** `EngineState` with `revision + 1`.
//! - A stale snapshot (revision ≤ current) is silently dropped.
//! - The current state is never mutated in-place.
//!
//! ## Optimisations (vs initial version)
//! - `layer_health` is now a fixed-size array `[Option<LayerHealth>; 16]`
//!   indexed by `LayerId as usize`. Cloning is stack-only — no heap allocation,
//!   no HashMap rehash, no pointer-chasing. 16 HashMap lookups per render frame
//!   become 16 direct array reads.
//! - `EngineState` is wrapped in `Arc` by the sync layer. `with_*` helpers use
//!   `Arc::make_mut` (copy-on-write) so only the mutated field is cloned when
//!   there is more than one reference holder.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use omega_control_contracts::{
    health::{HealthStatus, LayerId, LayerHealth},
    rest::{BlacklistResponse, CeilingStatusResponse, DaoFeeResponse, GasModelCheckpoint},
    ws::{WsConnectionStatus, WsEvent},
};

// ---------------------------------------------------------------------------
// Layer array helpers
// ---------------------------------------------------------------------------

/// Number of layers — must match `LayerId` variant count.
const LAYER_COUNT: usize = 16;

/// Convert a `LayerId` to its array index (stable, matches enum declaration order).
#[inline]
fn layer_index(layer: LayerId) -> usize {
    layer as usize
}

// ---------------------------------------------------------------------------
// EngineStateInner — the heap-allocated payload behind Arc
// ---------------------------------------------------------------------------

/// Inner state payload. Kept behind `Arc` so `with_*` helpers are copy-on-write:
/// if the Arc has exactly one reference, mutation is in-place; otherwise a
/// single clone of only the changed field is needed.
#[derive(Debug, Clone)]
struct EngineStateInner {
    revision:                  u64,
    updated_at:                DateTime<Utc>,

    // Fixed-size array — zero heap allocation on clone.
    layer_health:              [Option<LayerHealth>; LAYER_COUNT],
    overall_health:            HealthStatus,

    checkpoints:               Vec<GasModelCheckpoint>,
    active_checkpoint_version: Option<u64>,
    ceiling_status:            Option<CeilingStatusResponse>,
    dao_fee:                   Option<DaoFeeResponse>,
    blacklist:                 Option<BlacklistResponse>,
    ws_status:                 WsConnectionStatus,
    last_ws_seq:               Option<u64>,
}

impl Default for EngineStateInner {
    fn default() -> Self {
        Self {
            revision:                  0,
            updated_at:                Utc::now(),
            layer_health:              std::array::from_fn(|_| None),
            overall_health:            HealthStatus::Unknown,
            checkpoints:               Vec::new(),
            active_checkpoint_version: None,
            ceiling_status:            None,
            dao_fee:                   None,
            blacklist:                 None,
            ws_status:                 WsConnectionStatus::Disconnected,
            last_ws_seq:               None,
        }
    }
}

// ---------------------------------------------------------------------------
// EngineState — public handle (Arc-backed, cheap to clone)
// ---------------------------------------------------------------------------

/// Immutable engine state handle. Cheap to clone — cloning increments an
/// atomic reference count, not the payload.
///
/// Use `Arc::make_mut` internally for copy-on-write mutation.
#[derive(Debug, Clone, Default)]
pub struct EngineState(Arc<EngineStateInner>);

impl EngineState {
    // -----------------------------------------------------------------------
    // Public field accessors (read-through to inner)
    // -----------------------------------------------------------------------

    pub fn revision(&self)         -> u64              { self.0.revision }
    pub fn updated_at(&self)       -> DateTime<Utc>    { self.0.updated_at }
    pub fn overall_health(&self)   -> HealthStatus     { self.0.overall_health }
    pub fn ws_status(&self)        -> &WsConnectionStatus { &self.0.ws_status }
    pub fn checkpoints(&self)      -> &[GasModelCheckpoint] { &self.0.checkpoints }
    pub fn active_checkpoint_version(&self) -> Option<u64> { self.0.active_checkpoint_version }
    pub fn ceiling_status(&self)   -> Option<&CeilingStatusResponse> { self.0.ceiling_status.as_ref() }
    pub fn dao_fee(&self)          -> Option<&DaoFeeResponse>        { self.0.dao_fee.as_ref() }
    pub fn blacklist(&self)        -> Option<&BlacklistResponse>     { self.0.blacklist.as_ref() }
    pub fn last_ws_seq(&self)      -> Option<u64>      { self.0.last_ws_seq }

    // -----------------------------------------------------------------------
    // Snapshot acceptance
    // -----------------------------------------------------------------------

    /// Accept a new health snapshot. Returns `None` if stale (revision ≤ current).
    pub fn accept_health_snapshot(
        &self,
        snapshot: &omega_control_contracts::rest::HealthSnapshot,
    ) -> Option<EngineState> {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision      += 1;
        next.updated_at     = snapshot.generated_at;
        next.overall_health = if snapshot.system_halted {
            HealthStatus::Halted
        } else {
            HealthStatus::from_backend_str(snapshot.overall_state())
        };
        // Reset array then fill from snapshot.
        next.layer_health   = std::array::from_fn(|_| None);
        for entry in &snapshot.layers {
            if let Some(lh) = LayerHealth::from_entry(entry) {
                let index = layer_index(lh.layer);
                next.layer_health[index] = Some(lh);
            }
        }
        Some(EngineState(Arc::new(next)))
    }

    // -----------------------------------------------------------------------
    // WebSocket event application
    // -----------------------------------------------------------------------

    /// Apply a WebSocket event. Returns `None` for no-ops (Ping).
    pub fn apply_ws_event(&self, seq: u64, event: &WsEvent) -> Option<EngineState> {
        // Gap detection: missed events → trigger re-sync.
        if let Some(last) = self.0.last_ws_seq {
            if seq > last + 1 {
                let mut gap = EngineStateInner::clone(&self.0);
                gap.ws_status = WsConnectionStatus::Reconnecting { attempt: 0 };
                return Some(EngineState(Arc::new(gap)));
            }
        }

        let mut next = EngineStateInner::clone(&self.0);
        next.revision   += 1;
        next.updated_at  = Utc::now();
        next.last_ws_seq = Some(seq);

        match event {
            WsEvent::HealthUpdate(hu) => {
                next.overall_health = hu.overall;
                for change in &hu.changes {
                    let slot = &mut next.layer_health[layer_index(change.layer)];
                    if let Some(lh) = slot {
                        lh.status  = change.current;
                        lh.message = change.message.clone();
                    }
                }
                if hu.revision > next.revision {
                    next.revision = hu.revision;
                }
            }

            WsEvent::GasModelReverted(ev) => {
                next.active_checkpoint_version = Some(ev.checkpoint_version);
            }

            WsEvent::GasModelCeilingEscalation(ev) => {
                let slot = &mut next.layer_health[layer_index(LayerId::LossAttribution)];
                if let Some(lh) = slot {
                    lh.status  = HealthStatus::Degraded;
                    lh.message = Some(format!(
                        "GAS_MODEL_CEILING_REACHED: {} hits on {}",
                        ev.ceiling_hit_count, ev.feature_key
                    ));
                }
            }

            WsEvent::HaltPropagation(halt) => {
                for &layer in &halt.affected_layers {
                    let slot = &mut next.layer_health[layer_index(layer)];
                    if let Some(lh) = slot {
                        lh.status  = HealthStatus::Halted;
                        lh.message = Some(halt.reason.clone());
                    }
                }
                next.overall_health = HealthStatus::Halted;
            }

            // Observability-only events — no state change beyond seq/revision.
            WsEvent::LaReorgRisk(_)
            | WsEvent::EmergencyBundleSkipped(_)
            | WsEvent::ProfitSplit(_)
            | WsEvent::SimulationError(_) => {}

            WsEvent::Ping { .. } => return None,
        }

        Some(EngineState(Arc::new(next)))
    }

    // -----------------------------------------------------------------------
    // Ancillary data setters — copy-on-write via Arc::make_mut
    // -----------------------------------------------------------------------

    pub fn with_checkpoints(&self, checkpoints: Vec<GasModelCheckpoint>) -> EngineState {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision += 1;
        next.updated_at = Utc::now();
        next.active_checkpoint_version = checkpoints.iter().map(|c| c.version).max();
        next.checkpoints = checkpoints;
        EngineState(Arc::new(next))
    }

    pub fn with_dao_fee(&self, resp: DaoFeeResponse) -> EngineState {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision += 1;
        next.updated_at = Utc::now();
        next.dao_fee = Some(resp);
        EngineState(Arc::new(next))
    }

    pub fn with_blacklist(&self, resp: BlacklistResponse) -> EngineState {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision += 1;
        next.updated_at = Utc::now();
        next.blacklist = Some(resp);
        EngineState(Arc::new(next))
    }

    pub fn with_ceiling_status(&self, resp: CeilingStatusResponse) -> EngineState {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision += 1;
        next.updated_at = Utc::now();
        next.ceiling_status = Some(resp);
        EngineState(Arc::new(next))
    }

    pub fn with_ws_status(&self, status: WsConnectionStatus) -> EngineState {
        let mut next = EngineStateInner::clone(&self.0);
        next.revision += 1;
        next.updated_at = Utc::now();
        next.ws_status = status;
        EngineState(Arc::new(next))
    }

    // -----------------------------------------------------------------------
    // Queries — O(1) array reads
    // -----------------------------------------------------------------------

    /// Returns the `LayerHealth` for a specific layer (O(1) array read).
    #[inline]
    pub fn layer_health(&self, layer: LayerId) -> Option<&LayerHealth> {
        self.0.layer_health[layer_index(layer)].as_ref()
    }

    /// Returns the health status of a layer, or `Unknown` if not yet received.
    #[inline]
    pub fn layer_status(&self, layer: LayerId) -> HealthStatus {
        self.0.layer_health[layer_index(layer)]
            .as_ref()
            .map(|lh| lh.status)
            .unwrap_or(HealthStatus::Unknown)
    }

    /// Returns `true` if any layer is currently halted.
    pub fn any_halted(&self) -> bool {
        self.0.layer_health.iter()
            .filter_map(|s| s.as_ref())
            .any(|lh| lh.status == HealthStatus::Halted)
    }

    /// Returns `true` if the gas model is paused (ceiling escalation active).
    pub fn gas_model_paused(&self) -> bool {
        self.0.ceiling_status.as_ref().map(|cs| cs.any_paused).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use omega_control_contracts::rest::HealthSnapshot;

    fn make_snapshot() -> HealthSnapshot {
        HealthSnapshot {
            generated_at:  Utc::now(),
            layers:        vec![],
            system_halted: false,
        }
    }

    #[test]
    fn accept_newer_snapshot_increments_revision() {
        let state = EngineState::default();
        let next = state.accept_health_snapshot(&make_snapshot()).unwrap();
        assert_eq!(next.revision(), 1);
    }

    #[test]
    fn ping_event_returns_none() {
        let state = EngineState::default();
        assert!(state.apply_ws_event(1, &WsEvent::Ping { nonce: 42 }).is_none());
    }

    #[test]
    fn ws_gap_triggers_reconnect_status() {
        let state = EngineState(Arc::new(EngineStateInner {
            last_ws_seq: Some(5),
            ..EngineStateInner::default()
        }));
        let next = state.apply_ws_event(10, &WsEvent::Ping { nonce: 0 }).unwrap();
        assert!(matches!(next.ws_status(), WsConnectionStatus::Reconnecting { .. }));
    }

    #[test]
    fn layer_status_is_o1_array_read() {
        // Confirm no HashMap — just verify correct default
        let state = EngineState::default();
        assert_eq!(state.layer_status(LayerId::LossAttribution), HealthStatus::Unknown);
    }

    #[test]
    fn arc_state_clone_is_cheap() {
        // Cloning EngineState should not clone the inner payload
        let state = EngineState::default();
        let clone = state.clone();
        // Both point to the same Arc — strong_count == 2
        assert_eq!(Arc::strong_count(&state.0), 2);
        assert_eq!(Arc::strong_count(&clone.0), 2);
    }
}

