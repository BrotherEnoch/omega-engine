// crates/omega-risk/src/kill_switch.rs
//
// Absolute funds-at-risk kill switch (complements circuit_breakers.rs).
//
// circuit_breakers.rs answers: "is this strategy's EV ratio degrading
// relative to what we expected?" — a relative, strategy-specific signal
// that needs EV_WINDOW_BLOCKS of history to become reliable.
//
// This module answers a different question: "is real capital bleeding out
// right now, in absolute terms, faster than any bug should be allowed to
// bleed it?" It is deliberately simple, deliberately hard-coded in its
// thresholds (no adaptive/statistical smoothing), and deliberately sticky
// (no auto-resume) — the whole point is that a bug which fools the EV
// model can still be caught here, and a human has to look at it before
// anything resumes.
//
// Three independent trip conditions, any one of which halts:
//   1. Cumulative realized loss (all-time) >= max_cumulative_loss_wei.
//   2. Realized loss within a rolling time window >= max_loss_per_window_wei
//      (catches a fast bleed even if all-time cumulative loss is still
//      "acceptable" — e.g. a bug that starts losing money quickly after a
//      long healthy run).
//   3. N consecutive failed/reverted submissions in a row (catches a bug
//      that reverts every time, e.g. after a contract upgrade or an
//      exhausted allowance, before it costs a single further wei beyond
//      gas).
//
// Plus a manual kill switch (trip_manual) for a human to pull immediately,
// and a KillSwitchRegistry for per-strategy or engine-wide scoping,
// mirroring the CircuitBreakerRegistry pattern already in this crate.
//
// Metrics: KillSwitchRegistry (not the bare KillSwitch) is responsible for
// pushing state into omega_risk::metrics, since only the registry knows
// each switch's scope label. A bare KillSwitch used outside a registry
// carries no metrics side effects — see its doc comment.
//
// Reset audit trail: every reset() call, whether it originates from
// KillSwitch::reset directly or via KillSwitchRegistry::reset, is an
// event someone should be able to see without having to be told about it
// manually. KillSwitchRegistry::reset increments
// KILL_SWITCH_RESET_TOTAL{scope} (a Counter, so `increase()` over any
// window reliably surfaces the event even if scraped well after the
// fact) and sets KILL_SWITCH_RESET_LAST_OPERATOR_INFO{scope, operator,
// reason} to 1 as an "info metric" carrying the audit trail as label
// values. See docs/runbooks/kill-switch-tripped.md and the
// KillSwitchResetOccurred alert in ops/alerts/omega-risk.yaml.
//
// Diagnostics: `diagnostics()` (on both KillSwitch and
// KillSwitchRegistry) is the concrete counterpart to
// circuit_breakers.rs's BreakerDiagnostics — it's the answer to what
// docs/runbooks/kill-switch-tripped.md's diagnosis section previously
// described only as "pull the timestamped list of outcomes" with no
// concrete way to do so. It snapshots current status, cumulative loss,
// consecutive-failure streak, the raw loss entries still inside the
// configured window, and the switch's own config, in one call — so a
// responder can see exactly what's inside the window that produced a
// WindowLoss trip, not just the aggregate gauge value.
//
// Unlike StrategyCircuitBreaker, a bare KillSwitch has no scope label of
// its own (see the metrics note above), so `KillSwitch::diagnostics`
// takes `scope: &str` as a parameter purely to stamp it onto the
// returned struct — the registry passes its own key through when it
// delegates. This mirrors how `trip_manual`/`reset` already take
// `operator`/`reason` as caller-supplied parameters rather than storing
// them.
//
// ## Audit fix (this revision): deprecated chrono API
//
// All four call sites in this crate (three here, one in heartbeat.rs)
// that fell back to `chrono::Duration::max_value()` when converting a
// `std::time::Duration` that overflows `chrono::Duration`'s internal
// range now use `chrono::Duration::MAX` instead. `chrono::Duration` is a
// type alias for `TimeDelta`; `max_value()` is chrono's deprecated
// pre-associated-const spelling of the same value now exposed as the
// `MAX` const. Purely a deprecation-warning fix — the fallback value
// itself (used only when a configured window/silence duration is larger
// than chrono can represent, which no realistic config here approaches)
// is unchanged.

use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

use crate::metrics;

#[derive(Debug, Error)]
pub enum KillSwitchError {
    #[error("kill switch tripped: {0}")]
    Tripped(String),

    #[error("cannot reset: kill switch is not currently tripped")]
    NotTripped,

    #[error("invalid kill switch configuration: {0}")]
    InvalidConfig(String),
}

pub type Result<T> = std::result::Result<T, KillSwitchError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchConfig {
    /// Absolute all-time realized loss cap, in wei. Trips once cumulative
    /// realized loss reaches this magnitude, regardless of how long it
    /// took to get there.
    pub max_cumulative_loss_wei: u128,

    /// Realized loss cap within `loss_window`, in wei. Trips if losses
    /// within any rolling window of this duration reach this magnitude —
    /// this is what catches a *fast* bleed, independent of the all-time
    /// total.
    pub max_loss_per_window_wei: u128,

    /// Duration of the rolling window used for the above check.
    pub loss_window: Duration,

    /// Trip after this many consecutive failed/reverted outcomes, even if
    /// each one only cost gas. A strategy that reverts every single time
    /// is broken; letting it keep firing just burns gas for no benefit and
    /// usually signals something worse (bad state assumptions, a changed
    /// contract) that's worth stopping to look at.
    pub max_consecutive_failures: u32,
}

impl KillSwitchConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_cumulative_loss_wei == 0 {
            return Err(KillSwitchError::InvalidConfig(
                "max_cumulative_loss_wei must be > 0 (0 would trip immediately on any loss)".into(),
            ));
        }
        if self.max_loss_per_window_wei == 0 {
            return Err(KillSwitchError::InvalidConfig(
                "max_loss_per_window_wei must be > 0".into(),
            ));
        }
        if self.loss_window.is_zero() {
            return Err(KillSwitchError::InvalidConfig(
                "loss_window must be > 0".into(),
            ));
        }
        if self.max_consecutive_failures == 0 {
            return Err(KillSwitchError::InvalidConfig(
                "max_consecutive_failures must be >= 1".into(),
            ));
        }
        Ok(())
    }
}

/// One recorded execution outcome, kept only long enough to serve the
/// rolling-window loss check; evicted once older than `loss_window`.
#[derive(Debug, Clone, Copy)]
struct TimedLoss {
    at: DateTime<Utc>,
    /// Positive magnitude of loss for this outcome; 0 for a profitable or
    /// break-even outcome.
    loss_wei: u128,
}

/// One loss entry still inside the configured window, as returned by
/// `diagnostics()`. Public counterpart of the internal `TimedLoss` —
/// kept as a separate type rather than making `TimedLoss` itself public
/// so the internal representation (which includes entries already past
/// the window, pending eviction on the next write) stays decoupled from
/// what diagnostics reports (which is always filtered to "currently
/// inside the window," computed fresh at read time — see
/// `KillSwitch::diagnostics_at`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowLossEntry {
    pub at: DateTime<Utc>,
    pub loss_wei: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TripReason {
    CumulativeLoss {
        threshold_wei: u128,
        realized_loss_wei: u128,
    },
    WindowLoss {
        threshold_wei: u128,
        realized_loss_wei: u128,
        window_secs: u64,
    },
    ConsecutiveFailures {
        threshold: u32,
        observed: u32,
    },
    Manual {
        reason: String,
    },
}

impl std::fmt::Display for TripReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TripReason::CumulativeLoss { threshold_wei, realized_loss_wei } => write!(
                f,
                "cumulative realized loss {realized_loss_wei} wei reached/exceeded threshold {threshold_wei} wei"
            ),
            TripReason::WindowLoss { threshold_wei, realized_loss_wei, window_secs } => write!(
                f,
                "realized loss {realized_loss_wei} wei within last {window_secs}s reached/exceeded threshold {threshold_wei} wei"
            ),
            TripReason::ConsecutiveFailures { threshold, observed } => write!(
                f,
                "{observed} consecutive failures reached/exceeded threshold {threshold}"
            ),
            TripReason::Manual { reason } => write!(f, "manually tripped: {reason}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TripEvent {
    pub reason: TripReason,
    pub tripped_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KillSwitchStatus {
    Armed,
    Tripped(TripEvent),
}

impl KillSwitchStatus {
    pub fn is_tripped(&self) -> bool {
        matches!(self, KillSwitchStatus::Tripped(_))
    }
}

/// Full diagnostic snapshot for one scope's kill switch. Concrete
/// counterpart of `circuit_breakers::BreakerDiagnostics` — see the
/// module-level doc comment above for why this exists and what it
/// replaces in the runbook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchDiagnostics {
    pub scope: String,
    pub status: KillSwitchStatus,
    /// All-time realized loss, in wei — same value `cumulative_loss_wei()`
    /// and the `omega_risk_kill_switch_cumulative_loss_wei` gauge report.
    pub cumulative_loss_wei: u128,
    pub consecutive_failures: u32,
    /// Loss entries currently inside the configured `loss_window`,
    /// oldest first, computed fresh as of the snapshot time (not merely
    /// whatever was left over from the last `record_outcome` call's
    /// eviction pass — a scope that's gone quiet for a while would
    /// otherwise show stale entries that have actually aged out). Sum
    /// these to reconstruct the exact `realized_loss_wei` a `WindowLoss`
    /// trip would report right now.
    pub window_losses: Vec<WindowLossEntry>,
    /// The switch's own configuration, so a responder can see the exact
    /// thresholds being evaluated against without a second lookup.
    pub config: KillSwitchConfig,
}

struct State {
    cumulative_loss_wei: u128,
    window_history: VecDeque<TimedLoss>,
    consecutive_failures: u32,
    trip: Option<TripEvent>,
}

/// A single kill switch instance. Cheap to check (`guard`) before every
/// submission; cheap to update (`record_outcome`) after every receipt.
/// Once tripped, stays tripped until a human calls `reset` — there is no
/// automatic cooldown, because a bug that trips this is by definition not
/// yet understood, and auto-resuming it is exactly the failure mode this
/// exists to prevent.
///
/// NOTE: this type has no scope/label of its own and therefore never
/// touches `omega_risk::metrics` directly. Production code should go
/// through `KillSwitchRegistry`, which knows each switch's scope string
/// and keeps the Prometheus gauges/counters (including the reset audit
/// trail) in sync on every state change. A bare `KillSwitch` is fully
/// correct for logic/tests but is metrics-silent by design — don't build
/// a second, unscoped call path into a live engine.
pub struct KillSwitch {
    config: KillSwitchConfig,
    state: Mutex<State>,
}

impl KillSwitch {
    pub fn new(config: KillSwitchConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            config,
            state: Mutex::new(State {
                cumulative_loss_wei: 0,
                window_history: VecDeque::new(),
                consecutive_failures: 0,
                trip: None,
            }),
        })
    }

    pub fn config(&self) -> &KillSwitchConfig {
        &self.config
    }

    /// Returns `Ok(())` if submissions are currently permitted, or
    /// `Err(KillSwitchError::Tripped(..))` otherwise. Call immediately
    /// before submitting any bundle — this should be the very last check
    /// before a real transaction leaves the process.
    pub fn guard(&self) -> Result<()> {
        let state = self.state.lock();
        match &state.trip {
            Some(event) => Err(KillSwitchError::Tripped(event.reason.to_string())),
            None => Ok(()),
        }
    }

    pub fn status(&self) -> KillSwitchStatus {
        let state = self.state.lock();
        match &state.trip {
            Some(event) => KillSwitchStatus::Tripped(event.clone()),
            None => KillSwitchStatus::Armed,
        }
    }

    /// Full diagnostic snapshot — see `KillSwitchDiagnostics`. `scope` is
    /// stamped onto the result as-is (this type has no scope of its own —
    /// see the module doc comment); pass whatever string identifies this
    /// switch in your registry, or any placeholder if called on a bare
    /// `KillSwitch` outside a registry. Takes the state lock once, so
    /// status/loss/failures/window entries in the result are mutually
    /// consistent.
    pub fn diagnostics(&self, scope: &str) -> KillSwitchDiagnostics {
        self.diagnostics_at(scope, Utc::now())
    }

    /// Same as `diagnostics`, but with an explicit timestamp for the
    /// window-loss filtering, so the "which entries are still inside the
    /// window" computation is deterministically testable without
    /// sleeping in tests — mirrors `record_outcome_at`.
    pub fn diagnostics_at(&self, scope: &str, now: DateTime<Utc>) -> KillSwitchDiagnostics {
        let state = self.state.lock();

        let window =
            chrono::Duration::from_std(self.config.loss_window).unwrap_or(chrono::Duration::MAX);
        let window_losses: Vec<WindowLossEntry> = state
            .window_history
            .iter()
            .filter(|t| now - t.at <= window)
            .map(|t| WindowLossEntry {
                at: t.at,
                loss_wei: t.loss_wei,
            })
            .collect();

        let status = match &state.trip {
            Some(event) => KillSwitchStatus::Tripped(event.clone()),
            None => KillSwitchStatus::Armed,
        };

        KillSwitchDiagnostics {
            scope: scope.to_string(),
            status,
            cumulative_loss_wei: state.cumulative_loss_wei,
            consecutive_failures: state.consecutive_failures,
            window_losses,
            config: self.config.clone(),
        }
    }

    /// Records an outcome and evaluates all three automatic trip
    /// conditions. `realized_profit_wei` is signed (negative = loss),
    /// matching the convention used elsewhere in this crate (e.g.
    /// `omega-simulation::Receipt::realized_profit_wei`). Returns
    /// `Some(TripReason)` only if this call caused the trip just now.
    pub fn record_outcome(
        &self,
        realized_profit_wei: Option<i128>,
        success: bool,
    ) -> Option<TripReason> {
        self.record_outcome_at(realized_profit_wei, success, Utc::now())
    }

    /// Same as `record_outcome` but with an explicit timestamp, so the
    /// rolling-window logic is deterministically testable without
    /// sleeping in tests.
    pub fn record_outcome_at(
        &self,
        realized_profit_wei: Option<i128>,
        success: bool,
        now: DateTime<Utc>,
    ) -> Option<TripReason> {
        let mut state = self.state.lock();

        // Update consecutive-failure streak.
        if success {
            state.consecutive_failures = 0;
        } else {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        }

        // Update cumulative + windowed loss tracking. Only negative
        // realized_profit_wei counts as loss; profitable or unmeasured
        // (None) outcomes contribute 0 to both loss trackers but still
        // count toward the consecutive-failure streak via `success`.
        let loss_wei: u128 = realized_profit_wei
            .filter(|p| *p < 0)
            .map(|p| p.unsigned_abs())
            .unwrap_or(0);

        state.cumulative_loss_wei = state.cumulative_loss_wei.saturating_add(loss_wei);
        state
            .window_history
            .push_back(TimedLoss { at: now, loss_wei });
        Self::evict_expired(&mut state.window_history, now, self.config.loss_window);

        if state.trip.is_some() {
            // Keep recording for the audit trail, but the first trip is
            // sticky — don't let a later evaluation overwrite it.
            return None;
        }

        if let Some(reason) = self.check_cumulative_loss(&state) {
            return Some(self.apply_trip(&mut state, reason, now));
        }
        if let Some(reason) = self.check_window_loss(&state, now) {
            return Some(self.apply_trip(&mut state, reason, now));
        }
        if let Some(reason) = self.check_consecutive_failures(&state) {
            return Some(self.apply_trip(&mut state, reason, now));
        }

        None
    }

    fn evict_expired(history: &mut VecDeque<TimedLoss>, now: DateTime<Utc>, window: Duration) {
        let window = chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);
        while let Some(front) = history.front() {
            if now - front.at > window {
                history.pop_front();
            } else {
                break;
            }
        }
    }

    fn check_cumulative_loss(&self, state: &State) -> Option<TripReason> {
        if state.cumulative_loss_wei >= self.config.max_cumulative_loss_wei {
            Some(TripReason::CumulativeLoss {
                threshold_wei: self.config.max_cumulative_loss_wei,
                realized_loss_wei: state.cumulative_loss_wei,
            })
        } else {
            None
        }
    }

    fn check_window_loss(&self, state: &State, now: DateTime<Utc>) -> Option<TripReason> {
        let window =
            chrono::Duration::from_std(self.config.loss_window).unwrap_or(chrono::Duration::MAX);
        let window_loss: u128 = state
            .window_history
            .iter()
            .filter(|t| now - t.at <= window)
            .map(|t| t.loss_wei)
            .sum();
        if window_loss >= self.config.max_loss_per_window_wei {
            Some(TripReason::WindowLoss {
                threshold_wei: self.config.max_loss_per_window_wei,
                realized_loss_wei: window_loss,
                window_secs: self.config.loss_window.as_secs(),
            })
        } else {
            None
        }
    }

    fn check_consecutive_failures(&self, state: &State) -> Option<TripReason> {
        if state.consecutive_failures >= self.config.max_consecutive_failures {
            Some(TripReason::ConsecutiveFailures {
                threshold: self.config.max_consecutive_failures,
                observed: state.consecutive_failures,
            })
        } else {
            None
        }
    }

    fn apply_trip(&self, state: &mut State, reason: TripReason, now: DateTime<Utc>) -> TripReason {
        let event = TripEvent {
            reason: reason.clone(),
            tripped_at: now,
        };
        tracing::error!(reason = %event.reason, "kill switch tripped");
        state.trip = Some(event);
        reason
    }

    /// Immediately trips the switch regardless of recorded history — the
    /// manual kill switch. `operator` and `reason` are required so the
    /// trip carries an audit trail.
    pub fn trip_manual(&self, operator: &str, reason: &str) {
        let mut state = self.state.lock();
        let event = TripEvent {
            reason: TripReason::Manual {
                reason: format!("{reason} (operator: {operator})"),
            },
            tripped_at: Utc::now(),
        };
        tracing::error!(reason = %event.reason, "kill switch manually tripped");
        state.trip = Some(event);
    }

    /// Clears a trip so submissions resume. Requires the switch to
    /// currently be tripped. Does NOT reset cumulative loss or the
    /// consecutive-failure counter — if the underlying bug hasn't
    /// actually been fixed, the very next recorded outcome can retrip
    /// immediately, which is the intended behavior.
    ///
    /// This method itself does not touch `omega_risk::metrics` — it has
    /// no scope label. `KillSwitchRegistry::reset` is what pushes the
    /// reset audit-trail metrics (`KILL_SWITCH_RESET_TOTAL`,
    /// `KILL_SWITCH_RESET_LAST_OPERATOR_INFO`); calling this directly on
    /// a bare `KillSwitch` bypasses that trail, same as every other
    /// metrics-related caveat on this type.
    pub fn reset(&self, operator: &str, reason: &str) -> Result<()> {
        let mut state = self.state.lock();
        if state.trip.is_none() {
            return Err(KillSwitchError::NotTripped);
        }
        tracing::warn!(operator, reason, "kill switch manually reset");
        state.trip = None;
        state.consecutive_failures = 0;
        Ok(())
    }

    pub fn cumulative_loss_wei(&self) -> u128 {
        self.state.lock().cumulative_loss_wei
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.state.lock().consecutive_failures
    }
}

/// Per-scope registry (e.g. one `KillSwitch` per strategy ID, plus one
/// keyed "GLOBAL" for engine-wide capital protection), mirroring
/// `CircuitBreakerRegistry`'s pattern in `circuit_breakers.rs`.
///
/// This is the metrics-aware entry point: every method that can change a
/// switch's tripped state or loss totals also pushes the corresponding
/// `omega_risk::metrics` gauges/counters, so a live dashboard/alert never
/// needs to poll `guard()`/`status()` directly, and a reset is visible to
/// the whole team automatically rather than depending on the resetting
/// operator remembering to announce it.
#[derive(Clone)]
pub struct KillSwitchRegistry {
    switches: Arc<DashMap<String, Arc<KillSwitch>>>,
    default_config: KillSwitchConfig,
}

impl KillSwitchRegistry {
    pub fn new(default_config: KillSwitchConfig) -> Result<Self> {
        default_config.validate()?;
        Ok(Self {
            switches: Arc::new(DashMap::new()),
            default_config,
        })
    }

    /// Bug fix: this previously read `!self.switches.contains_key(scope)`
    /// into an `is_new` bool, then made a *separate* `entry(..).
    /// or_insert_with(..)` call and acted on that stale bool afterward.
    /// Two threads racing on the same never-seen `scope` could both
    /// observe `is_new == true` before either had inserted — the losing
    /// thread's `or_insert_with` becomes a no-op (it gets the winner's
    /// switch back), but it would still fall into the "publish initial
    /// metrics" branch and re-zero `KILL_SWITCH_TRIPPED` /
    /// `KILL_SWITCH_CUMULATIVE_LOSS_WEI` for a switch that, by the time
    /// the loser ran, might already have been tripped or accumulated
    /// real loss by the winner.
    ///
    /// Matching on `dashmap::Entry` directly removes the separate read:
    /// "did I just create this entry" is now inherent to a single atomic
    /// map operation, so the initial-metrics branch can only ever execute
    /// for the one thread that actually inserted it.
    fn get_or_create(&self, scope: &str) -> Arc<KillSwitch> {
        match self.switches.entry(scope.to_string()) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => {
                let switch = Arc::new(
                    KillSwitch::new(self.default_config.clone())
                        .expect("default_config already validated in KillSwitchRegistry::new"),
                );
                e.insert(switch.clone());

                // Publish the configured threshold once, at first observation
                // of this scope, so alert rules comparing observed loss
                // against the threshold (e.g. "80% of cap reached") have
                // something to compare against from the start rather than
                // only after the first recorded outcome.
                metrics::KILL_SWITCH_MAX_CUMULATIVE_LOSS_WEI
                    .with_label_values(&[scope])
                    .set(switch.config().max_cumulative_loss_wei as f64);
                metrics::KILL_SWITCH_TRIPPED
                    .with_label_values(&[scope])
                    .set(0.0);
                metrics::KILL_SWITCH_CUMULATIVE_LOSS_WEI
                    .with_label_values(&[scope])
                    .set(0.0);

                switch
            }
        }
    }

    /// True if `scope` may currently submit. Unknown scopes default to
    /// armed-and-permitted (they get a fresh switch on first outcome).
    pub fn guard(&self, scope: &str) -> Result<()> {
        self.get_or_create(scope).guard()
    }

    /// Records an outcome for `scope` and syncs metrics gauges regardless
    /// of whether this call caused a trip, since cumulative loss can
    /// change on every call even without tripping.
    pub fn record_outcome(
        &self,
        scope: &str,
        realized_profit_wei: Option<i128>,
        success: bool,
    ) -> Option<TripReason> {
        let switch = self.get_or_create(scope);
        let result = switch.record_outcome(realized_profit_wei, success);

        metrics::KILL_SWITCH_CUMULATIVE_LOSS_WEI
            .with_label_values(&[scope])
            .set(switch.cumulative_loss_wei() as f64);

        if result.is_some() {
            metrics::KILL_SWITCH_TRIPPED
                .with_label_values(&[scope])
                .set(1.0);
        }

        result
    }

    pub fn status(&self, scope: &str) -> KillSwitchStatus {
        self.get_or_create(scope).status()
    }

    /// Full diagnostic snapshot for one scope — see
    /// `KillSwitchDiagnostics`. Returns `None` if `scope` was never
    /// registered, mirroring `CircuitBreakerRegistry::diagnostics`:
    /// unlike `guard`/`status`, which default an unknown scope to a
    /// healthy-looking value so a strategy's first-ever trade isn't
    /// blocked, a responder explicitly asking for diagnostics on a scope
    /// that doesn't exist should see that plainly rather than a
    /// misleadingly "armed and fine" snapshot. Deliberately reads the
    /// underlying map directly instead of going through `get_or_create`,
    /// so calling this never has the side effect of registering a new
    /// scope or pushing its initial metrics.
    pub fn diagnostics(&self, scope: &str) -> Option<KillSwitchDiagnostics> {
        self.switches.get(scope).map(|s| s.diagnostics(scope))
    }

    /// Same as `diagnostics`, but with an explicit timestamp — see
    /// `KillSwitch::diagnostics_at`.
    pub fn diagnostics_at(&self, scope: &str, now: DateTime<Utc>) -> Option<KillSwitchDiagnostics> {
        self.switches
            .get(scope)
            .map(|s| s.diagnostics_at(scope, now))
    }

    /// Diagnostic snapshots for every registered scope, for a
    /// control-plane dashboard or a bulk incident-response check.
    pub fn all_diagnostics(&self) -> Vec<KillSwitchDiagnostics> {
        self.switches
            .iter()
            .map(|e| e.value().diagnostics(e.key()))
            .collect()
    }

    pub fn trip_manual(&self, scope: &str, operator: &str, reason: &str) {
        self.get_or_create(scope).trip_manual(operator, reason);
        metrics::KILL_SWITCH_TRIPPED
            .with_label_values(&[scope])
            .set(1.0);
    }

    /// Clears a trip for `scope`. On success:
    ///   - Resets the tripped gauge to 0.
    ///   - Increments `KILL_SWITCH_RESET_TOTAL{scope}` — a Counter, so
    ///     this reset is durably visible in `increase()` queries even if
    ///     scraped well after the fact, closing the gap both runbooks
    ///     flagged ("post manually in the team channel" is no longer
    ///     required for this to be visible).
    ///   - Sets `KILL_SWITCH_RESET_LAST_OPERATOR_INFO{scope, operator,
    ///     reason}` to 1, so the *most recent* operator/reason pair is
    ///     queryable directly from Prometheus without needing to grep
    ///     application logs for the `tracing::warn!` emitted inside
    ///     `KillSwitch::reset`.
    ///
    /// The cumulative-loss gauge is deliberately left untouched, since
    /// `KillSwitch::reset` itself does not clear cumulative loss, and the
    /// metric should keep reflecting reality (a reset switch with high
    /// historical loss should still show that loss on a dashboard).
    pub fn reset(&self, scope: &str, operator: &str, reason: &str) -> Result<()> {
        self.get_or_create(scope).reset(operator, reason)?;

        metrics::KILL_SWITCH_TRIPPED
            .with_label_values(&[scope])
            .set(0.0);
        metrics::KILL_SWITCH_RESET_TOTAL
            .with_label_values(&[scope])
            .inc();
        // Info-metric pattern: this is a new distinct label combination on
        // every reset with a different operator/reason (Prometheus has no
        // native "overwrite the previous series" primitive), so old
        // operator/reason series for this scope remain queryable but
        // stale at value 1 forever. That's an accepted tradeoff of the
        // info-metric pattern generally; if a given scope resets very
        // frequently with many distinct reasons, this can accumulate
        // low-value stale series over time. KILL_SWITCH_RESET_TOTAL
        // remains the reliable source for "how many times," this info
        // metric is a convenience for "who/why most recently" and should
        // be cross-checked against tracing/application logs if precision
        // matters for an audit.
        metrics::KILL_SWITCH_RESET_LAST_OPERATOR_INFO
            .with_label_values(&[scope, operator, reason])
            .set(1.0);

        Ok(())
    }

    /// Trips every registered scope at once — the engine-wide "stop
    /// everything" button, distinct from tripping just one strategy.
    pub fn trip_all(&self, operator: &str, reason: &str) {
        for entry in self.switches.iter() {
            let scope = entry.key().clone();
            entry.value().trip_manual(operator, reason);
            metrics::KILL_SWITCH_TRIPPED
                .with_label_values(&[scope.as_str()])
                .set(1.0);
        }
        tracing::error!(operator, reason, "kill switch: ALL scopes manually tripped");
    }

    /// Snapshot of all scopes' status, for a control-plane dashboard.
    pub fn all_statuses(&self) -> Vec<(String, KillSwitchStatus)> {
        self.switches
            .iter()
            .map(|e| (e.key().clone(), e.value().status()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KillSwitchConfig {
        KillSwitchConfig {
            max_cumulative_loss_wei: 1_000_000_000_000_000_000, // 1 ETH all-time
            max_loss_per_window_wei: 200_000_000_000_000_000,   // 0.2 ETH per window
            loss_window: Duration::from_secs(3600),             // 1 hour
            max_consecutive_failures: 5,
        }
    }

    #[test]
    fn rejects_invalid_config() {
        let mut bad = cfg();
        bad.max_cumulative_loss_wei = 0;
        assert!(KillSwitch::new(bad).is_err());
    }

    #[test]
    fn starts_armed() {
        let k = KillSwitch::new(cfg()).unwrap();
        assert!(k.guard().is_ok());
        assert!(!k.status().is_tripped());
    }

    #[test]
    fn trips_on_cumulative_loss() {
        // Window cap set effectively-disabled so cumulative is what trips
        // (each individual 0.4 ETH loss would otherwise blow the 0.2 ETH
        // window cap first).
        let mut c = cfg();
        c.max_loss_per_window_wei = 10_000_000_000_000_000_000; // 10 ETH
        let k = KillSwitch::new(c).unwrap();
        assert!(k
            .record_outcome(Some(-400_000_000_000_000_000), true)
            .is_none());
        assert!(k
            .record_outcome(Some(-400_000_000_000_000_000), true)
            .is_none());
        let tripped = k.record_outcome(Some(-400_000_000_000_000_000), true);
        assert!(matches!(tripped, Some(TripReason::CumulativeLoss { .. })));
        assert!(k.guard().is_err());
    }

    #[test]
    fn trips_on_window_loss_even_when_cumulative_still_low() {
        // Cumulative cap high, window cap low and tight — simulates a
        // sudden fast bleed that hasn't yet reached the all-time cap.
        let mut c = cfg();
        c.max_cumulative_loss_wei = 100_000_000_000_000_000_000; // 100 ETH
        c.max_loss_per_window_wei = 300_000_000_000_000_000; // 0.3 ETH
        c.loss_window = Duration::from_secs(600); // 10 min
        let k = KillSwitch::new(c).unwrap();

        let t0 = Utc::now();
        assert!(k
            .record_outcome_at(Some(-100_000_000_000_000_000), true, t0)
            .is_none());
        assert!(k
            .record_outcome_at(
                Some(-100_000_000_000_000_000),
                true,
                t0 + chrono::Duration::seconds(60)
            )
            .is_none());
        let tripped = k.record_outcome_at(
            Some(-100_000_000_000_000_000),
            true,
            t0 + chrono::Duration::seconds(120),
        );
        assert!(matches!(tripped, Some(TripReason::WindowLoss { .. })));
    }

    #[test]
    fn old_losses_outside_window_are_evicted_and_dont_count() {
        let mut c = cfg();
        c.max_cumulative_loss_wei = 100_000_000_000_000_000_000; // disabled
        c.max_loss_per_window_wei = 300_000_000_000_000_000; // 0.3 ETH
        c.loss_window = Duration::from_secs(600); // 10 min
        let k = KillSwitch::new(c).unwrap();

        let t0 = Utc::now();
        k.record_outcome_at(Some(-200_000_000_000_000_000), true, t0);
        k.record_outcome_at(
            Some(-200_000_000_000_000_000),
            true,
            t0 + chrono::Duration::seconds(60),
        );
        let later = t0 + chrono::Duration::seconds(1200);
        let tripped = k.record_outcome_at(Some(-100_000_000_000_000_000), true, later);
        assert!(tripped.is_none());
        assert!(k.guard().is_ok());
    }

    #[test]
    fn trips_on_consecutive_failures_even_with_zero_dollar_loss() {
        let k = KillSwitch::new(cfg()).unwrap();
        for _ in 0..4 {
            assert!(k.record_outcome(None, false).is_none());
        }
        let tripped = k.record_outcome(None, false);
        assert!(matches!(
            tripped,
            Some(TripReason::ConsecutiveFailures { .. })
        ));
        assert!(k.guard().is_err());
    }

    #[test]
    fn success_resets_consecutive_failure_streak() {
        let k = KillSwitch::new(cfg()).unwrap();
        for _ in 0..4 {
            k.record_outcome(None, false);
        }
        assert_eq!(k.consecutive_failures(), 4);
        k.record_outcome(Some(1), true);
        assert_eq!(k.consecutive_failures(), 0);
        for _ in 0..4 {
            assert!(k.record_outcome(None, false).is_none());
        }
        assert!(k.record_outcome(None, false).is_some());
    }

    #[test]
    fn manual_trip_is_immediate_and_independent_of_thresholds() {
        let k = KillSwitch::new(cfg()).unwrap();
        assert!(k.guard().is_ok());
        k.trip_manual("alice", "suspected reentrancy bug in LiquidationArb");
        assert!(k.guard().is_err());
        match k.status() {
            KillSwitchStatus::Tripped(e) => assert!(matches!(e.reason, TripReason::Manual { .. })),
            KillSwitchStatus::Armed => panic!("expected tripped"),
        }
    }

    #[test]
    fn reset_requires_tripped_state_and_clears_failure_streak() {
        let k = KillSwitch::new(cfg()).unwrap();
        assert!(k.reset("alice", "false alarm").is_err());

        for _ in 0..5 {
            k.record_outcome(None, false);
        }
        assert!(k.guard().is_err());
        assert!(k.reset("alice", "root cause fixed, redeployed").is_ok());
        assert!(k.guard().is_ok());
        assert_eq!(k.consecutive_failures(), 0);
    }

    #[test]
    fn trip_is_sticky_across_further_outcomes() {
        let k = KillSwitch::new(cfg()).unwrap();
        k.trip_manual("alice", "testing");
        assert!(k
            .record_outcome(Some(1_000_000_000_000_000_000), true)
            .is_none());
        assert!(k.guard().is_err());
    }

    #[test]
    fn diagnostics_reflects_cumulative_loss_and_failures() {
        let k = KillSwitch::new(cfg()).unwrap();
        k.record_outcome(Some(-500_000_000_000_000_000), true);
        k.record_outcome(None, false);
        k.record_outcome(None, false);

        let diag = k.diagnostics("TEST_SCOPE");
        assert_eq!(diag.scope, "TEST_SCOPE");
        assert_eq!(diag.cumulative_loss_wei, 500_000_000_000_000_000);
        assert_eq!(diag.consecutive_failures, 2);
        assert!(!diag.status.is_tripped());
    }

    #[test]
    fn diagnostics_status_reflects_trip_reason() {
        let k = KillSwitch::new(cfg()).unwrap();
        k.trip_manual("alice", "suspected bug");
        let diag = k.diagnostics("TEST_SCOPE");
        match diag.status {
            KillSwitchStatus::Tripped(event) => {
                assert!(matches!(event.reason, TripReason::Manual { .. }))
            }
            KillSwitchStatus::Armed => panic!("expected tripped"),
        }
    }

    #[test]
    fn diagnostics_window_losses_only_includes_entries_within_window() {
        let mut c = cfg();
        c.loss_window = Duration::from_secs(600); // 10 min
        let k = KillSwitch::new(c).unwrap();

        let t0 = Utc::now();
        // One loss well inside the window as of t0, one loss that will
        // have aged out by the time we snapshot at t0 + 20min.
        k.record_outcome_at(Some(-100_000_000_000_000_000), true, t0);
        let diag_at_t0 = k.diagnostics_at("TEST_SCOPE", t0 + chrono::Duration::seconds(1));
        assert_eq!(diag_at_t0.window_losses.len(), 1);

        // Snapshot taken well past the window, WITHOUT any intervening
        // record_outcome call — confirms filtering happens at read time,
        // not only at the last write's eviction pass.
        let diag_later = k.diagnostics_at("TEST_SCOPE", t0 + chrono::Duration::seconds(1200));
        assert_eq!(
            diag_later.window_losses.len(),
            0,
            "diagnostics must filter by window at read time, not rely on stale internal eviction"
        );
        // Cumulative loss is unaffected by window aging — still shows the
        // all-time total.
        assert_eq!(diag_later.cumulative_loss_wei, 100_000_000_000_000_000);
    }

    #[test]
    fn diagnostics_includes_config() {
        let c = cfg();
        let k = KillSwitch::new(c.clone()).unwrap();
        let diag = k.diagnostics("TEST_SCOPE");
        assert_eq!(
            diag.config.max_cumulative_loss_wei,
            c.max_cumulative_loss_wei
        );
        assert_eq!(
            diag.config.max_consecutive_failures,
            c.max_consecutive_failures
        );
    }

    #[test]
    fn registry_scopes_are_independent() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.trip_manual("LA", "alice", "LA-specific bug");
        assert!(reg.guard("LA").is_err());
        assert!(reg.guard("SA").is_ok());
    }

    #[test]
    fn registry_trip_all_halts_every_scope() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.record_outcome("LA", Some(1), true);
        reg.record_outcome("SA", Some(1), true);
        reg.record_outcome("MEV", Some(1), true);
        assert!(reg.guard("LA").is_ok());

        reg.trip_all("alice", "emergency: suspected oracle manipulation");

        assert!(reg.guard("LA").is_err());
        assert!(reg.guard("SA").is_err());
        assert!(reg.guard("MEV").is_err());
    }

    #[test]
    fn registry_unknown_scope_defaults_armed() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        assert!(reg.guard("NEVER_SEEN_BEFORE").is_ok());
    }

    #[test]
    fn registry_diagnostics_returns_none_for_unregistered_scope() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        assert!(reg.diagnostics("NEVER_SEEN").is_none());
    }

    #[test]
    fn registry_diagnostics_does_not_auto_register() {
        // Confirms diagnostics() has no side effect of creating a scope —
        // unlike guard()/record_outcome()/trip_manual(), which all go
        // through get_or_create. Calling diagnostics() on an unseen scope
        // twice should both times return None, not register it on the
        // first call.
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        assert!(reg.diagnostics("PROBE").is_none());
        assert!(reg.diagnostics("PROBE").is_none());
    }

    #[test]
    fn registry_diagnostics_matches_scope_after_trip() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.trip_manual("MEV", "alice", "test trip for diagnostics");
        let diag = reg
            .diagnostics("MEV")
            .expect("scope should be registered after trip_manual");
        assert_eq!(diag.scope, "MEV");
        assert!(diag.status.is_tripped());
    }

    #[test]
    fn registry_all_diagnostics_returns_every_registered_scope() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.record_outcome("LA", Some(1), true);
        reg.record_outcome("SA", Some(1), true);
        reg.record_outcome("MEV", Some(1), true);
        let all = reg.all_diagnostics();
        assert_eq!(all.len(), 3);
        let scopes: Vec<_> = all.iter().map(|d| d.scope.as_str()).collect();
        assert!(scopes.contains(&"LA"));
        assert!(scopes.contains(&"SA"));
        assert!(scopes.contains(&"MEV"));
    }

    #[test]
    fn registry_syncs_cumulative_loss_gauge_on_every_record() {
        // Metrics registries are process-global (Lazy statics), so this
        // test only checks that record_outcome doesn't panic when pushing
        // gauge updates and that repeated calls with distinct scopes don't
        // interfere with each other's guard() results.
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.record_outcome("SCOPE_A", Some(-1_000), true);
        reg.record_outcome("SCOPE_B", Some(-2_000), true);
        assert!(reg.guard("SCOPE_A").is_ok());
        assert!(reg.guard("SCOPE_B").is_ok());
    }

    #[test]
    fn registry_reset_clears_tripped_gauge_without_panicking() {
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        reg.trip_manual("SCOPE_C", "alice", "test");
        assert!(reg.guard("SCOPE_C").is_err());
        reg.reset("SCOPE_C", "alice", "resolved").unwrap();
        assert!(reg.guard("SCOPE_C").is_ok());
    }

    #[test]
    fn registry_reset_increments_reset_counter() {
        // Uses a distinct scope name so this test's counter value isn't
        // polluted by other tests running against the same process-global
        // Lazy metric — CounterVec is keyed by label, so a fresh scope
        // string starts its series at 0 regardless of test execution order.
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        let scope = "SCOPE_RESET_COUNTER_TEST";
        reg.trip_manual(scope, "alice", "test");
        reg.reset(scope, "alice", "first reset").unwrap();
        reg.trip_manual(scope, "bob", "test again");
        reg.reset(scope, "bob", "second reset").unwrap();

        let count = metrics::KILL_SWITCH_RESET_TOTAL
            .with_label_values(&[scope])
            .get();
        assert_eq!(count, 2.0);
    }

    #[test]
    fn registry_reset_errors_do_not_increment_counter() {
        // reset() on a switch that isn't tripped should error before ever
        // touching the metrics — confirms KillSwitchRegistry::reset's `?`
        // short-circuit actually happens before the metric calls, not
        // after.
        let reg = KillSwitchRegistry::new(cfg()).unwrap();
        let scope = "SCOPE_RESET_ERROR_TEST";
        reg.get_or_create(scope); // register without tripping
        assert!(reg
            .reset(scope, "alice", "premature reset attempt")
            .is_err());

        let count = metrics::KILL_SWITCH_RESET_TOTAL
            .with_label_values(&[scope])
            .get();
        assert_eq!(count, 0.0);
    }
}
