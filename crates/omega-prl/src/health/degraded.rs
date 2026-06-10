// omega-prl/src/health/degraded.rs
//! PRL health FSM — §17.1
//!
//! State transitions are monotonically degrading (Healthy→Degraded→Limited→Halted)
//! except for explicit governance-approved recovery.
//! All state reads are lock-free (AtomicU8).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use tracing::{error, info, warn};

/// PRL health state (§17.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PrlHealthState {
    Healthy = 0,
    Degraded = 1,
    Limited = 2,
    Halted = 3,
}

impl PrlHealthState {
    #[inline]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Healthy,
            1 => Self::Degraded,
            2 => Self::Limited,
            3 => Self::Halted,
            _ => Self::Degraded,
        }
    }

    /// Advisory signals are available in all states except Halted.
    #[inline]
    pub fn is_advisory_active(self) -> bool {
        !matches!(self, Self::Halted)
    }
}

/// Thread-safe PRL health tracker.
pub struct PrlHealth {
    state: AtomicU8,
    reason: parking_lot::RwLock<String>,
    degradation_count: AtomicU64,
}

impl PrlHealth {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(PrlHealthState::Healthy as u8),
            reason: parking_lot::RwLock::new("initial".into()),
            degradation_count: AtomicU64::new(0),
        }
    }

    /// Current state — lock-free.
    #[inline]
    pub fn state(&self) -> PrlHealthState {
        PrlHealthState::from_u8(self.state.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn is_halted(&self) -> bool {
        self.state.load(Ordering::Relaxed) == PrlHealthState::Halted as u8
    }

    #[inline]
    pub fn is_ml_active(&self) -> bool {
        matches!(self.state(), PrlHealthState::Healthy)
    }

    /// Degrade to DEGRADED.  Never upgrades state.
    pub fn set_degraded(&self, reason: &str) {
        let new = match self.state() {
            PrlHealthState::Healthy => PrlHealthState::Degraded,
            other => other,
        };
        self.transition_to(new, reason);
    }

    /// Degrade to LIMITED (heuristic-only).  Never upgrades.
    pub fn set_limited(&self, reason: &str) {
        let new = match self.state() {
            PrlHealthState::Halted => PrlHealthState::Halted,
            _ => PrlHealthState::Limited,
        };
        self.transition_to(new, reason);
    }

    /// Transition to HALTED.  Only governance `recover()` can undo this.
    pub fn halt(&self, reason: &str) {
        self.transition_to(PrlHealthState::Halted, reason);
        error!(reason, "PRL HALTED — all advisory outputs disabled");
    }

    /// Governance-approved recovery to HEALTHY.
    pub fn recover(&self, approved_by: &str) {
        self.state
            .store(PrlHealthState::Healthy as u8, Ordering::SeqCst);
        *self.reason.write() = format!("recovered by {approved_by}");
        self.degradation_count.store(0, Ordering::Relaxed);
        info!(approved_by, "PRL health recovered to HEALTHY");
    }

    pub fn degradation_count(&self) -> u64 {
        self.degradation_count.load(Ordering::Relaxed)
    }

    pub fn reason(&self) -> String {
        self.reason.read().clone()
    }

    fn transition_to(&self, new: PrlHealthState, reason: &str) {
        let old = PrlHealthState::from_u8(self.state.swap(new as u8, Ordering::SeqCst));
        if old != new {
            *self.reason.write() = reason.to_string();
            self.degradation_count.fetch_add(1, Ordering::Relaxed);
            warn!(old = ?old, new = ?new, reason, "PRL health state transition");
        }
    }
}

impl Default for PrlHealth {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_is_healthy() {
        let h = PrlHealth::new();
        assert_eq!(h.state(), PrlHealthState::Healthy);
        assert!(!h.is_halted());
    }

    #[test]
    fn set_degraded_from_healthy() {
        let h = PrlHealth::new();
        h.set_degraded("test");
        assert_eq!(h.state(), PrlHealthState::Degraded);
    }

    #[test]
    fn set_degraded_does_not_upgrade_limited() {
        let h = PrlHealth::new();
        h.set_limited("limit");
        h.set_degraded("try upgrade");
        assert_eq!(h.state(), PrlHealthState::Limited);
    }

    #[test]
    fn halt_is_terminal_without_recover() {
        let h = PrlHealth::new();
        h.halt("test");
        h.set_degraded("ignored");
        assert_eq!(h.state(), PrlHealthState::Halted);
    }

    #[test]
    fn governance_recover_resets_count() {
        let h = PrlHealth::new();
        h.halt("test");
        h.recover("multisig");
        assert_eq!(h.state(), PrlHealthState::Healthy);
        assert_eq!(h.degradation_count(), 0);
    }

    #[test]
    fn advisory_active_except_halted() {
        assert!(PrlHealthState::Healthy.is_advisory_active());
        assert!(PrlHealthState::Degraded.is_advisory_active());
        assert!(PrlHealthState::Limited.is_advisory_active());
        assert!(!PrlHealthState::Halted.is_advisory_active());
    }
}
