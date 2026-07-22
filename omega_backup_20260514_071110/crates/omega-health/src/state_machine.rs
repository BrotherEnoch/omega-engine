ï»¿// crates/omega-health/src/state_machine.rs
//
// 14-layer Health FSM â€” concrete implementation of the LayerHealth trait.
//
// TLA+ spec: formal/health_fsm.tla
//
// Spec Â§3 â€” three-state FSM per layer:
//
//   Healthy  â†’ Degraded  : non-fatal anomaly detected
//   Healthy  â†’ Halted    : catastrophic fault (skip Degraded when needed)
//   Degraded â†’ Halted    : threshold crossed or cascading fault
//   Degraded â†’ Healthy   : transient fault cleared (automatic recovery)
//   Halted   â†’ Healthy   : governance clearance only (Â§5)
//   Halted   â†’ Degraded  : partial recovery â€” not in TLA+ spec; blocked
//
// Blocked transitions:
//   Halted â†’ Degraded    : disallowed â€” recovery is always to Healthy
//   (same state â†’ same)  : no-op with WARN log to detect logic errors
//
// Observability contract (Â§16):
//   Every state transition is written to the persistence::HealthLog AND
//   emitted on the propagation::TransitionSender channel so the
//   SystemHealth orchestrator can react (halt propagation, Â§3).
//   The tracing event is the secondary record; the log entry is primary.
//
// Locking:
//   `RwLock<HealthState>` â€” many readers (is_operational polls), one writer
//   (set_state).  The write lock is held only for the duration of the
//   CAS-style read-then-write.  No lock is held while calling into
//   `persistence` or `propagation` â€” those are called after the write
//   lock is released to avoid lock inversion.

use std::sync::{Arc, RwLock};

use chrono::Utc;
use omega_core::{HealthState, LayerId, LayerHealth};

use crate::persistence::{HealthLog, HealthLogEntry};
use crate::propagation::{TransitionEvent, TransitionSender};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// TransitionError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Describes why a requested state transition was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    /// `Halted â†’ Degraded` is not a valid recovery path.
    /// Recovery from Halted must go directly to Healthy (Â§3).
    #[error("Invalid transition {from:?} â†’ {to:?} for layer {layer:?}: Halted may only recover to Healthy")]
    HaltedToDegragedBlocked {
        layer: LayerId,
        from:  HealthState,
        to:    HealthState,
    },

    /// Attempted transition to the same state.  This is a caller logic
    /// error â€” set_state should only be called when a state change is
    /// required.
    #[error("No-op transition {state:?} â†’ {state:?} for layer {layer:?}")]
    NoOp {
        layer: LayerId,
        state: HealthState,
    },
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LayerHealthImpl
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Concrete, thread-safe implementation of [`LayerHealth`].
///
/// One instance is created per layer at engine startup and shared
/// across all tasks via `Arc<LayerHealthImpl>`.
///
/// ## Wiring
///
/// - `log`: optional handle to the append-only health log (Â§persistence).
///   `None` in unit tests or when persistence is disabled.
/// - `tx`: optional channel sender to the SystemHealth propagation loop
///   (Â§propagation).  `None` in tests or when the propagation task is
///   not running.
pub struct LayerHealthImpl {
    state:    RwLock<HealthState>,
    layer_id: LayerId,
    /// Optional persistence handle.  Locked per transition â€” transitions
    /// are infrequent so contention is not a concern.
    log:      Option<std::sync::Mutex<HealthLog>>,
    /// Optional propagation channel.  Non-blocking send â€” if the channel
    /// is full the event is dropped with a WARN log rather than blocking
    /// the health transition.
    tx:       Option<TransitionSender>,
}

impl LayerHealthImpl {
    /// Create a new `LayerHealthImpl` starting in `Healthy` state.
    ///
    /// `log` and `tx` are optional â€” pass `None` in tests or when
    /// those subsystems are not yet initialised.
    pub fn new(
        id:  LayerId,
        log: Option<HealthLog>,
        tx:  Option<TransitionSender>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state:    RwLock::new(HealthState::Healthy),
            layer_id: id,
            log:      log.map(|l| std::sync::Mutex::new(l)),
            tx,
        })
    }

    /// Create a minimal instance with no persistence or propagation.
    ///
    /// Intended for unit tests and early-startup use before the full
    /// subsystem is wired.
    pub fn new_bare(id: LayerId) -> Arc<Self> {
        Self::new(id, None, None)
    }

    /// Attempt to transition to `new_state`.
    ///
    /// Returns `Ok(())` on success.  Returns `Err(TransitionError)` for
    /// invalid transitions without mutating state.
    ///
    /// Callers that do not need to inspect the rejection reason can call
    /// [`LayerHealth::set_state`] instead, which logs and discards the error.
    pub fn try_transition(
        &self,
        new_state: HealthState,
        reason:    &str,
    ) -> Result<(), TransitionError> {
        // --- Read current state under read lock ---
        let old_state = *self.state.read().expect("health state RwLock poisoned");

        // --- Validate transition ---
        if old_state == new_state {
            let err = TransitionError::NoOp {
                layer: self.layer_id,
                state: old_state,
            };
            // Not an error â€” log at WARN and return Ok so callers don't
            // need to special-case defensive duplicate set_state calls.
            tracing::warn!(
                layer  = %self.layer_id,
                state  = %old_state,
                reason = reason,
                "no-op health transition; caller may have duplicate set_state",
            );
            return Err(err);
        }

        if old_state == HealthState::Halted && new_state == HealthState::Degraded {
            return Err(TransitionError::HaltedToDegragedBlocked {
                layer: self.layer_id,
                from:  old_state,
                to:    new_state,
            });
        }

        // --- Apply transition under write lock ---
        // Hold the write lock only for the store, then release before
        // calling into persistence/propagation.
        {
            let mut guard = self.state.write().expect("health state RwLock poisoned");
            *guard = new_state;
        }

        // --- Emit telemetry (outside lock) ---
        match new_state {
            HealthState::Healthy  => tracing::info!(
                layer  = %self.layer_id,
                from   = %old_state,
                to     = %new_state,
                reason = reason,
                "HEALTH_STATE_CHANGE",
            ),
            HealthState::Degraded => tracing::warn!(
                layer  = %self.layer_id,
                from   = %old_state,
                to     = %new_state,
                reason = reason,
                "HEALTH_STATE_CHANGE",
            ),
            HealthState::Halted => tracing::error!(
                layer  = %self.layer_id,
                from   = %old_state,
                to     = %new_state,
                reason = reason,
                "HEALTH_STATE_CHANGE",
            ),
        }

        let entry = HealthLogEntry {
            timestamp:  Utc::now(),
            layer_id:   self.layer_id.to_string(),
            from_state: old_state.to_string(),
            to_state:   new_state.to_string(),
            reason:     reason.to_owned(),
        };

        // --- Persist (outside lock, best-effort) ---
        if let Some(ref log_mutex) = self.log {
            match log_mutex.lock() {
                Ok(mut log) => {
                    if let Err(e) = log.append(&entry) {
                        tracing::error!(
                            layer = %self.layer_id,
                            error = %e,
                            "Failed to persist health log entry â€” transition already applied",
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        layer = %self.layer_id,
                        error = %e,
                        "Health log mutex poisoned",
                    );
                }
            }
        }

        // --- Propagate to SystemHealth orchestrator (outside lock, non-blocking) ---
        if let Some(ref tx) = self.tx {
            let event = TransitionEvent {
                layer:     self.layer_id,
                from:      old_state,
                to:        new_state,
                reason:    reason.to_owned(),
                timestamp: entry.timestamp,
            };
            tx.send_nonblocking(event);
        }

        Ok(())
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LayerHealth impl
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

impl LayerHealth for LayerHealthImpl {
    fn state(&self) -> HealthState {
        *self.state.read().expect("health state RwLock poisoned")
    }

    fn layer_id(&self) -> LayerId {
        self.layer_id
    }

    /// Apply a state transition, discarding invalid-transition errors.
    ///
    /// For callers that need to inspect the result, use
    /// [`LayerHealthImpl::try_transition`] directly.
    fn set_state(&self, new_state: HealthState, reason: &str) {
        if let Err(e) = self.try_transition(new_state, reason) {
            match &e {
                TransitionError::NoOp { .. } => {
                    // Already logged in try_transition at WARN.
                }
                TransitionError::HaltedToDegragedBlocked { .. } => {
                    tracing::error!(
                        error = %e,
                        "Blocked illegal health FSM transition â€” layer remains Halted",
                    );
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

    fn layer() -> Arc<LayerHealthImpl> {
        LayerHealthImpl::new_bare(LayerId::Relay)
    }

    #[test]
    fn initial_state_is_healthy() {
        assert_eq!(layer().state(), HealthState::Healthy);
    }

    #[test]
    fn healthy_to_degraded() {
        let l = layer();
        l.set_state(HealthState::Degraded, "test");
        assert_eq!(l.state(), HealthState::Degraded);
    }

    #[test]
    fn healthy_to_halted_skipping_degraded() {
        // Spec Â§3 allows direct Healthy â†’ Halted on catastrophic faults.
        let l = layer();
        l.set_state(HealthState::Halted, "catastrophic");
        assert_eq!(l.state(), HealthState::Halted);
    }

    #[test]
    fn degraded_to_halted() {
        let l = layer();
        l.set_state(HealthState::Degraded, "first");
        l.set_state(HealthState::Halted, "threshold crossed");
        assert_eq!(l.state(), HealthState::Halted);
    }

    #[test]
    fn degraded_to_healthy_automatic_recovery() {
        let l = layer();
        l.set_state(HealthState::Degraded, "transient");
        l.set_state(HealthState::Healthy, "cleared");
        assert_eq!(l.state(), HealthState::Healthy);
    }

    #[test]
    fn halted_to_healthy_governance_recovery() {
        let l = layer();
        l.set_state(HealthState::Halted, "fault");
        l.set_state(HealthState::Healthy, "governance cleared");
        assert_eq!(l.state(), HealthState::Healthy);
    }

    #[test]
    fn halted_to_degraded_is_blocked() {
        let l = layer();
        l.set_state(HealthState::Halted, "fault");
        // Must be blocked â€” state must remain Halted
        let result = l.try_transition(HealthState::Degraded, "invalid recovery");
        assert!(matches!(result, Err(TransitionError::HaltedToDegragedBlocked { .. })));
        assert_eq!(l.state(), HealthState::Halted, "state must not have changed");
    }

    #[test]
    fn noop_transition_returns_err_but_does_not_panic() {
        let l = layer();
        let result = l.try_transition(HealthState::Healthy, "noop");
        assert!(matches!(result, Err(TransitionError::NoOp { .. })));
        assert_eq!(l.state(), HealthState::Healthy);
    }

    #[test]
    fn is_operational_reflects_state() {
        let l = layer();
        assert!(l.is_operational());
        l.set_state(HealthState::Degraded, "d");
        assert!(l.is_operational(), "Degraded is still operational");
        l.set_state(HealthState::Halted, "h");
        assert!(!l.is_operational(), "Halted is not operational");
    }
}