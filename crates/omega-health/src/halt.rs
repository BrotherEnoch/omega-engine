// crates/omega-health/src/halt.rs
//
// HaltFlag — system-wide emergency halt mechanism.
//
// Spec §3:
//   The L0 Health FSM may issue an EMERGENCY_HALT that must propagate to
//   every active execution loop within 200ms.  Relay and scoring loops
//   poll the halt flag every 10ms (100 polls/second), guaranteeing the
//   200ms SLA is met with ≥19× polling margin.
//
// Memory ordering rationale:
//   `halt()` uses `SeqCst` store: a halt is a safety-critical write that
//   must be immediately visible across all threads with no reordering.
//
//   `is_halted()` uses `Acquire` load: sufficient to observe any `SeqCst`
//   or `Release` store.  `SeqCst` on the read path would be correct but
//   wastes a full memory barrier on every 10ms poll — `Acquire` is the
//   minimum correct ordering for the read side of a halt check.
//
//   `clear()` uses `SeqCst`: clearing a halt is as safety-critical as
//   setting one — it must be totally ordered with respect to any
//   concurrent halt stores.
//
// Cloning semantics:
//   `HaltFlag` wraps `Arc<AtomicBool>`.  All clones share the same
//   underlying flag — there is exactly one halt state per engine
//   instance.  The orchestrator holds the canonical instance;
//   relay/scoring loops hold clones.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use chrono::{DateTime, Utc};

// ─────────────────────────────────────────────────────────────────────────────
// HaltFlag
// ─────────────────────────────────────────────────────────────────────────────

/// System-wide emergency halt flag.
///
/// Polled by relay and scoring loops every 10ms.  A halt issued by the
/// L0 Health FSM reaches all loops within 200ms (spec §3).
///
/// All clones share the same underlying `AtomicBool` via `Arc` — there
/// is exactly one halt state per engine instance.
#[derive(Clone, Debug)]
pub struct HaltFlag {
    flag: Arc<AtomicBool>,
    /// Timestamp of the most recent `halt()` call.  `None` if never
    /// halted, or if `clear()` has been called since the last halt.
    /// Stored in a separate `Arc<parking_lot::Mutex<Option<…>>>` so
    /// that clones share the timestamp too.
    ///
    /// We use `std::sync::Mutex` (not parking_lot) to stay within
    /// workspace dependencies.  The mutex is only locked during
    /// `halt()` and `clear()` — never on the hot poll path.
    halted_at: Arc<std::sync::Mutex<Option<HaltRecord>>>,
}

/// Record of a single halt event.
#[derive(Debug, Clone)]
pub struct HaltRecord {
    /// Wall-clock time the halt was issued.
    pub timestamp: DateTime<Utc>,
    /// Human-readable reason supplied by the caller.
    pub reason: String,
    /// Layer that issued the halt.
    pub issuer: omega_core::LayerId,
}

impl HaltFlag {
    /// Create a new, non-halted flag.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            halted_at: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Issue an EMERGENCY_HALT.
    ///
    /// Sets the atomic flag with `SeqCst` ordering and records the halt
    /// timestamp and reason.  Emits a tracing event at ERROR level.
    ///
    /// Safe to call from any thread; safe to call multiple times (the
    /// flag is already set after the first call, subsequent calls
    /// overwrite the record with the latest reason).
    pub fn halt(&self, issuer: omega_core::LayerId, reason: &str) {
        self.flag.store(true, Ordering::SeqCst);

        let record = HaltRecord {
            timestamp: Utc::now(),
            reason: reason.to_owned(),
            issuer,
        };

        // Record under lock — only on the slow halt path, never polled.
        if let Ok(mut guard) = self.halted_at.lock() {
            *guard = Some(record);
        }

        tracing::error!(
            layer  = %issuer,
            reason = reason,
            "EMERGENCY_HALT issued",
        );
    }

    /// Clear the halt flag.
    ///
    /// Must only be called by the SystemHealth orchestrator after
    /// governance clearance (§3).  Emits a tracing event at WARN level
    /// so the clear is always visible in the audit log.
    pub fn clear(&self, cleared_by: omega_core::LayerId, reason: &str) {
        self.flag.store(false, Ordering::SeqCst);

        if let Ok(mut guard) = self.halted_at.lock() {
            *guard = None;
        }

        tracing::warn!(
            cleared_by = %cleared_by,
            reason     = reason,
            "EMERGENCY_HALT cleared",
        );
    }

    /// Poll the halt flag.
    ///
    /// Called every 10ms by relay and scoring loops.  Uses `Acquire`
    /// ordering — the minimum correct ordering to observe a `SeqCst`
    /// store without paying for an unnecessary full barrier on every poll.
    #[inline]
    pub fn is_halted(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Return the halt record if currently halted, or `None`.
    ///
    /// Acquires a mutex lock — do NOT call on the hot poll path.  Use
    /// `is_halted()` for polling; use this only for diagnostics and
    /// logging.
    pub fn halt_record(&self) -> Option<HaltRecord> {
        self.halted_at.lock().ok()?.clone()
    }
}

impl Default for HaltFlag {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omega_core::LayerId;

    #[test]
    fn initially_not_halted() {
        let f = HaltFlag::new();
        assert!(!f.is_halted());
        assert!(f.halt_record().is_none());
    }

    #[test]
    fn halt_sets_flag_and_records() {
        let f = HaltFlag::new();
        f.halt(LayerId::SystemHealth, "test halt");
        assert!(f.is_halted());
        let rec = f.halt_record().expect("record present after halt");
        assert_eq!(rec.issuer, LayerId::SystemHealth);
        assert_eq!(rec.reason, "test halt");
    }

    #[test]
    fn clear_resets_flag_and_record() {
        let f = HaltFlag::new();
        f.halt(LayerId::SystemHealth, "test");
        f.clear(LayerId::SystemHealth, "governance cleared");
        assert!(!f.is_halted());
        assert!(f.halt_record().is_none());
    }

    #[test]
    fn clone_shares_state() {
        let a = HaltFlag::new();
        let b = a.clone();
        a.halt(LayerId::Relay, "clone test");
        assert!(b.is_halted(), "clone must observe halt from original");
        b.clear(LayerId::SystemHealth, "clearing via clone");
        assert!(!a.is_halted(), "original must observe clear from clone");
    }

    #[test]
    fn multiple_halts_overwrite_record() {
        let f = HaltFlag::new();
        f.halt(LayerId::Relay, "first");
        f.halt(LayerId::Security, "second");
        let rec = f.halt_record().unwrap();
        assert_eq!(rec.reason, "second");
        assert_eq!(rec.issuer, LayerId::Security);
    }
}
