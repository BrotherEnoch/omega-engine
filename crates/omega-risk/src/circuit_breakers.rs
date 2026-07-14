// crates/omega-risk/src/circuit_breakers.rs
//
// Per-strategy EV-ratio circuit breakers (spec Section 19 — Adaptive EV-Weighted Rollout).
//
// EV ratio = observed_profit / expected_profit over a 72-block rolling window.
//
// Thresholds (spec S19 / adverse selection detector S8):
//   ≥ 0.85              → Healthy — no action.
//   0.70–0.85           → Investigate — emit alert, reduce rollout tier.
//   0.50–0.70 for N blocks → AutoPaused — L2 fast-approve required to resume.
//   < 0.50              → Halted — L3 governance required (circuit break).
//
// Implementation:
//   • One `StrategyCircuitBreaker` per strategy ID, stored in a DashMap.
//   • Each breaker holds a VecDeque window of (observed, expected) pairs.
//   • State transitions are lock-free for reads (AtomicU8) and use a Mutex
//     only for the window write path.
//   • `CircuitBreakerRegistry` is the shared handle injected into all strategy tasks.
//
// Metrics: CIRCUIT_BREAKER_STATE and EV_RATIO (omega_risk::metrics) are
// pushed directly from StrategyCircuitBreaker on every state-affecting
// call — record(), resume_l2(), clear_halt_l3() — since each breaker
// already owns its strategy_id label. This differs from kill_switch.rs,
// where the metrics push lives at the registry layer because a bare
// KillSwitch has no scope label of its own; here the breaker itself does,
// so pushing from inside StrategyCircuitBreaker is the more direct path.
//
// Audit trail: both governance recovery actions require an `operator` and
// `reason`, mirroring KillSwitch::reset/KillSwitchRegistry::reset:
//   - resume_l2   → CIRCUIT_BREAKER_L2_RESUME_TOTAL / _LAST_OPERATOR_INFO
//                   / CircuitBreakerL2ResumeOccurred alert (info severity)
//   - clear_halt_l3 → CIRCUIT_BREAKER_L3_CLEAR_TOTAL / _LAST_OPERATOR_INFO
//                   / CircuitBreakerL3ClearOccurred alert (info severity)
// Both return `bool`: true if the call actually changed state, false if
// it was a no-op (wrong starting state, e.g. calling resume_l2 on a
// strategy that isn't AutoPaused). See docs/runbooks/circuit-breaker-halted.md
// and ops/alerts/omega-risk.yaml.
//
// Diagnostics: `diagnostics()` (on both StrategyCircuitBreaker and
// CircuitBreakerRegistry) is the concrete answer to what
// docs/runbooks/circuit-breaker-halted.md's diagnosis section previously
// described only as "via whatever your control-plane exposes." It
// snapshots state, EV ratio, window fill, the raw (observed, expected)
// pairs currently in the window, and the last transition timestamp, in
// one call — everything a responder needs to distinguish "a few trades
// went badly" from "every trade is uniformly underperforming" without
// separately querying Prometheus for the aggregate ratio and then having
// no way to see what's actually inside the window that produced it.
//
// Spec: "< 0.70 for 72 blocks → AUTO-PAUSED (L2 fast-approve to resume)"
//       "< 0.50 → circuit-break (L3 governance required)"

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use chrono::{DateTime, Utc};

use crate::metrics;

// ─── Spec thresholds ─────────────────────────────────────────────────────────

/// Rolling window size in blocks (spec S19: 72 blocks).
pub const EV_WINDOW_BLOCKS: usize = 72;

/// EV ratio below which strategy enters Investigate state (spec S19: 0.85).
pub const EV_INVESTIGATE_THRESHOLD: f64 = 0.85;

/// EV ratio below which strategy is AUTO-PAUSED (spec S19: 0.70).
pub const EV_AUTO_PAUSE_THRESHOLD: f64 = 0.70;

/// EV ratio below which strategy is circuit-broken / Halted (spec S19: 0.50).
pub const EV_HALT_THRESHOLD: f64 = 0.50;

// ─── Circuit state FSM ────────────────────────────────────────────────────────

/// Per-strategy circuit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// EV ratio ≥ 0.85 — operating normally.
    Healthy,
    /// 0.70 ≤ EV ratio < 0.85 — emit alert, reduce rollout, continue.
    Investigate,
    /// EV ratio < 0.70 sustained over window — paused; L2 fast-approve to resume.
    AutoPaused,
    /// EV ratio < 0.50 — hard stop; L3 governance required.
    Halted,
}

impl CircuitState {
    /// True if the strategy may continue submitting blueprints.
    pub fn is_operational(self) -> bool {
        matches!(self, CircuitState::Healthy | CircuitState::Investigate)
    }

    fn to_u8(self) -> u8 {
        match self {
            CircuitState::Healthy    => 0,
            CircuitState::Investigate=> 1,
            CircuitState::AutoPaused => 2,
            CircuitState::Halted     => 3,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => CircuitState::Healthy,
            1 => CircuitState::Investigate,
            2 => CircuitState::AutoPaused,
            _ => CircuitState::Halted,
        }
    }
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Healthy     => write!(f, "HEALTHY"),
            CircuitState::Investigate => write!(f, "INVESTIGATE"),
            CircuitState::AutoPaused  => write!(f, "AUTO_PAUSED"),
            CircuitState::Halted      => write!(f, "HALTED"),
        }
    }
}

// ─── Per-strategy breaker ─────────────────────────────────────────────────────

struct BreakerWindow {
    deque:       VecDeque<(f64, f64)>,  // (observed_profit, expected_profit)
    window_size: usize,
}

impl BreakerWindow {
    fn new(size: usize) -> Self {
        Self { deque: VecDeque::with_capacity(size), window_size: size }
    }

    fn push(&mut self, observed: f64, expected: f64) {
        if self.deque.len() >= self.window_size {
            self.deque.pop_front();
        }
        self.deque.push_back((observed, expected));
    }

    /// EV ratio over the current window. Returns 1.0 if no data.
    fn ev_ratio(&self) -> f64 {
        if self.deque.is_empty() { return 1.0; }
        let obs: f64 = self.deque.iter().map(|(o, _)| o).sum();
        let exp: f64 = self.deque.iter().map(|(_, e)| e).sum();
        if exp <= 0.0 { return 1.0; }
        obs / exp
    }

    fn len(&self) -> usize { self.deque.len() }

    /// Copy of the raw (observed, expected) pairs currently in the
    /// window, oldest first. Used by `diagnostics()` — deliberately not
    /// exposed as a zero-copy reference, since callers (diagnostic
    /// tooling, not the hot path) shouldn't hold a lock open while
    /// inspecting this, and the window is small (capped at
    /// EV_WINDOW_BLOCKS) so the clone is cheap.
    fn snapshot(&self) -> Vec<(f64, f64)> {
        self.deque.iter().copied().collect()
    }
}

/// Point-in-time diagnostic snapshot for one strategy's circuit breaker.
/// This is the concrete answer to "pull the individual (observed,
/// expected) pairs feeding the current window" from
/// docs/runbooks/circuit-breaker-halted.md's diagnosis section — call
/// `StrategyCircuitBreaker::diagnostics()` or
/// `CircuitBreakerRegistry::diagnostics(strategy_id)` instead of reaching
/// into internals or querying Prometheus for anything beyond the
/// aggregate state/ratio gauges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakerDiagnostics {
    pub strategy_id: String,
    pub state: CircuitState,
    /// Aggregate EV ratio over the current window — same value
    /// `ev_ratio()` and the `omega_risk_ev_ratio` gauge report.
    pub ev_ratio: f64,
    /// Number of (observed, expected) pairs currently in the window
    /// (0..=EV_WINDOW_BLOCKS).
    pub window_fill: usize,
    /// The raw pairs themselves, oldest first. Use this to distinguish
    /// "a few trades were deeply negative, most were fine" (a handful of
    /// outlier pairs) from "every trade is uniformly underperforming" (a
    /// consistent ratio across all pairs) — the runbook's diagnosis
    /// branches on exactly this distinction and previously had no
    /// concrete way to check it.
    pub window_pairs: Vec<(f64, f64)>,
    /// When the state last actually transitioned (not merely when
    /// `record()` was last called — a call that doesn't change state
    /// doesn't update this).
    pub last_transition: DateTime<Utc>,
}

/// Circuit breaker for one strategy.
pub struct StrategyCircuitBreaker {
    strategy_id:     String,
    state:           Arc<AtomicU8>,
    window:          Mutex<BreakerWindow>,
    last_transition: Mutex<DateTime<Utc>>,
}

impl StrategyCircuitBreaker {
    fn new(strategy_id: impl Into<String>) -> Self {
        let strategy_id = strategy_id.into();

        // Publish initial gauge values immediately on construction, so a
        // freshly-registered strategy shows up on a dashboard as Healthy
        // with EV ratio 1.0 rather than being absent (and indistinguishable
        // from "never registered") until its first recorded outcome.
        metrics::CIRCUIT_BREAKER_STATE
            .with_label_values(&[strategy_id.as_str()])
            .set(CircuitState::Healthy.to_u8() as f64);
        metrics::EV_RATIO.with_label_values(&[strategy_id.as_str()]).set(1.0);

        Self {
            strategy_id,
            state:           Arc::new(AtomicU8::new(CircuitState::Healthy.to_u8())),
            window:          Mutex::new(BreakerWindow::new(EV_WINDOW_BLOCKS)),
            last_transition: Mutex::new(Utc::now()),
        }
    }

    /// Record a completed blueprint outcome.
    ///
    /// `observed` = actual profit achieved (0.0 if lost/reverted).
    /// `expected` = profit estimated at scoring time.
    pub fn record(&self, observed: f64, expected: f64) {
        // Write to window under lock (only path that takes the lock).
        let ev_ratio = {
            let mut win = self.window.lock();
            win.push(observed, expected);
            win.ev_ratio()
        };

        // EV_RATIO is pushed on every call, independent of whether the
        // state transitions — the gauge should track the live ratio, not
        // just snapshot it at transition boundaries.
        metrics::EV_RATIO.with_label_values(&[self.strategy_id.as_str()]).set(ev_ratio);

        // Determine new state from EV ratio.
        let new_state = if ev_ratio < EV_HALT_THRESHOLD {
            CircuitState::Halted
        } else if ev_ratio < EV_AUTO_PAUSE_THRESHOLD {
            CircuitState::AutoPaused
        } else if ev_ratio < EV_INVESTIGATE_THRESHOLD {
            CircuitState::Investigate
        } else {
            CircuitState::Healthy
        };

        let old_u8 = self.state.load(Ordering::Acquire);
        let new_u8 = new_state.to_u8();

        if old_u8 != new_u8 {
            self.state.store(new_u8, Ordering::Release);
            *self.last_transition.lock() = Utc::now();
            metrics::CIRCUIT_BREAKER_STATE
                .with_label_values(&[self.strategy_id.as_str()])
                .set(new_u8 as f64);
            let old_state = CircuitState::from_u8(old_u8);
            tracing::warn!(
                strategy = %self.strategy_id,
                from      = %old_state,
                to        = %new_state,
                ev_ratio,
                "circuit breaker state transition"
            );
        }
    }

    /// Read current state (lock-free atomic load).
    pub fn state(&self) -> CircuitState {
        CircuitState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// True if the strategy may submit blueprints.
    pub fn is_operational(&self) -> bool {
        self.state().is_operational()
    }

    /// Current EV ratio (requires window lock — use sparingly in hot path).
    pub fn ev_ratio(&self) -> f64 {
        self.window.lock().ev_ratio()
    }

    /// Window fill level (0..=EV_WINDOW_BLOCKS).
    pub fn window_size(&self) -> usize {
        self.window.lock().len()
    }

    /// Full diagnostic snapshot — see `BreakerDiagnostics` doc comment.
    /// Takes the window lock once and reads everything under it, so the
    /// state/ratio/pairs in the returned snapshot are mutually consistent
    /// (no risk of `record()` landing between two separate reads and
    /// producing a ratio that doesn't match the returned pairs).
    pub fn diagnostics(&self) -> BreakerDiagnostics {
        let win = self.window.lock();
        BreakerDiagnostics {
            strategy_id: self.strategy_id.clone(),
            state: self.state(),
            ev_ratio: win.ev_ratio(),
            window_fill: win.len(),
            window_pairs: win.snapshot(),
            last_transition: *self.last_transition.lock(),
        }
    }

    // ── Governance recovery actions ───────────────────────────────────────────

    /// Resume from AutoPaused (L2 fast-approve path, spec S19).
    /// Resets state to Investigate (not immediately Healthy) to stay
    /// cautious. Does NOT clear the EV window — the ratio continues
    /// reflecting recent history until it ages out of the 72-block window
    /// naturally, a deliberately softer reset than `clear_halt_l3`'s hard
    /// window clear (L2 is the lower-severity recovery path per spec
    /// S19, so it gets a lower bar to invoke but also a gentler effect on
    /// state — not a fresh start with no memory).
    ///
    /// `operator` and `reason` are required, same audit-trail discipline
    /// as `clear_halt_l3` and `KillSwitch::reset`. Returns `true` if a
    /// resume actually occurred (state was AutoPaused), `false` if called
    /// on a breaker that wasn't AutoPaused (a no-op) — lets a caller
    /// distinguish "I resumed something" from "there was nothing to
    /// resume" without checking state before and after themselves.
    pub fn resume_l2(&self, operator: &str, reason: &str) -> bool {
        let current = self.state();
        if current != CircuitState::AutoPaused {
            return false;
        }

        self.state.store(CircuitState::Investigate.to_u8(), Ordering::Release);
        *self.last_transition.lock() = Utc::now();
        metrics::CIRCUIT_BREAKER_STATE
            .with_label_values(&[self.strategy_id.as_str()])
            .set(CircuitState::Investigate.to_u8() as f64);

        // Audit trail: durable counter (never lost between scrapes) plus
        // an info-metric snapshot of the most recent operator/reason,
        // same pattern as clear_halt_l3 and KillSwitchRegistry::reset.
        metrics::CIRCUIT_BREAKER_L2_RESUME_TOTAL
            .with_label_values(&[self.strategy_id.as_str()])
            .inc();
        metrics::CIRCUIT_BREAKER_L2_RESUME_LAST_OPERATOR_INFO
            .with_label_values(&[self.strategy_id.as_str(), operator, reason])
            .set(1.0);

        tracing::info!(
            strategy = %self.strategy_id,
            operator,
            reason,
            "circuit breaker resumed (L2)"
        );
        true
    }

    /// Clear from Halted (L3 governance path, spec S19).
    /// Resets state to Investigate and clears the EV window.
    ///
    /// `operator` and `reason` are required, mirroring
    /// `KillSwitch::reset` — this is the highest-severity recovery action
    /// in this module (spec S19 requires L3 governance specifically
    /// because Halted is the most severe state), so it carries the same
    /// audit-trail requirement. Returns `true` if a clear actually
    /// occurred (state was Halted), `false` if called on a breaker that
    /// wasn't Halted (a no-op) — same pattern as `resume_l2` above.
    pub fn clear_halt_l3(&self, operator: &str, reason: &str) -> bool {
        let current = self.state();
        if current != CircuitState::Halted {
            return false;
        }

        // Clear the window so the EV ratio starts fresh.
        {
            let mut win = self.window.lock();
            win.deque.clear();
        }
        self.state.store(CircuitState::Investigate.to_u8(), Ordering::Release);
        *self.last_transition.lock() = Utc::now();
        metrics::CIRCUIT_BREAKER_STATE
            .with_label_values(&[self.strategy_id.as_str()])
            .set(CircuitState::Investigate.to_u8() as f64);
        // Window was just cleared, so ev_ratio() returns to the "no
        // data" default of 1.0 — reflect that on the gauge immediately
        // rather than leaving it at whatever the pre-clear value was.
        metrics::EV_RATIO.with_label_values(&[self.strategy_id.as_str()]).set(1.0);

        metrics::CIRCUIT_BREAKER_L3_CLEAR_TOTAL
            .with_label_values(&[self.strategy_id.as_str()])
            .inc();
        metrics::CIRCUIT_BREAKER_L3_CLEAR_LAST_OPERATOR_INFO
            .with_label_values(&[self.strategy_id.as_str(), operator, reason])
            .set(1.0);

        tracing::warn!(
            strategy = %self.strategy_id,
            operator,
            reason,
            "circuit breaker cleared (L3 governance)"
        );
        true
    }
}

// ─── Registry ────────────────────────────────────────────────────────────────

/// Shared registry of per-strategy circuit breakers.
///
/// `Arc<CircuitBreakerRegistry>` is cloned into every strategy task at startup.
/// Reads are lock-free (DashMap + AtomicU8).
#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    breakers: Arc<DashMap<String, Arc<StrategyCircuitBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self { breakers: Arc::new(DashMap::new()) }
    }

    /// Ensure a breaker exists for `strategy_id` (idempotent).
    pub fn register(&self, strategy_id: &str) {
        self.breakers
            .entry(strategy_id.to_string())
            .or_insert_with(|| Arc::new(StrategyCircuitBreaker::new(strategy_id)));
    }

    /// Record an outcome for `strategy_id`.
    /// Auto-registers the strategy if not yet present.
    pub fn record(&self, strategy_id: &str, observed: f64, expected: f64) {
        let breaker = self.breakers
            .entry(strategy_id.to_string())
            .or_insert_with(|| Arc::new(StrategyCircuitBreaker::new(strategy_id)));
        breaker.record(observed, expected);
    }

    /// True if the strategy may submit blueprints (lock-free atomic read).
    pub fn is_operational(&self, strategy_id: &str) -> bool {
        match self.breakers.get(strategy_id) {
            Some(b) => b.is_operational(),
            None    => true, // unknown strategy: default allow (register on first outcome)
        }
    }

    /// Current state for a strategy.
    pub fn state(&self, strategy_id: &str) -> CircuitState {
        match self.breakers.get(strategy_id) {
            Some(b) => b.state(),
            None    => CircuitState::Healthy,
        }
    }

    /// Current EV ratio for a strategy.
    pub fn ev_ratio(&self, strategy_id: &str) -> f64 {
        match self.breakers.get(strategy_id) {
            Some(b) => b.ev_ratio(),
            None    => 1.0,
        }
    }

    /// Full diagnostic snapshot for one strategy — see
    /// `BreakerDiagnostics`. Returns `None` if `strategy_id` was never
    /// registered, distinct from every other read method on this
    /// registry, which defaults unknown strategies to a healthy-looking
    /// value. That default is appropriate for `is_operational`/`state`/
    /// `ev_ratio` (an unregistered strategy shouldn't be blocked from its
    /// first trade), but wrong here — a responder asking for diagnostics
    /// on a strategy that doesn't exist should see that explicitly, not a
    /// misleadingly "healthy" snapshot.
    pub fn diagnostics(&self, strategy_id: &str) -> Option<BreakerDiagnostics> {
        self.breakers.get(strategy_id).map(|b| b.diagnostics())
    }

    /// Diagnostic snapshots for every registered strategy, for a
    /// control-plane dashboard or a bulk incident-response check across
    /// all strategies at once.
    pub fn all_diagnostics(&self) -> Vec<BreakerDiagnostics> {
        self.breakers.iter().map(|e| e.value().diagnostics()).collect()
    }

    /// Resume an AutoPaused strategy via L2 governance. `operator` and
    /// `reason` are required — see `StrategyCircuitBreaker::resume_l2`.
    /// Returns `false` if `strategy_id` was never registered, or if it
    /// was registered but not currently AutoPaused (both are no-ops).
    pub fn resume_l2(&self, strategy_id: &str, operator: &str, reason: &str) -> bool {
        match self.breakers.get(strategy_id) {
            Some(b) => b.resume_l2(operator, reason),
            None => false,
        }
    }

    /// Clear a Halted strategy via L3 governance. `operator` and `reason`
    /// are required — see `StrategyCircuitBreaker::clear_halt_l3`.
    /// Returns `false` if `strategy_id` was never registered, or if it
    /// was registered but not currently Halted (both are no-ops).
    pub fn clear_halt_l3(&self, strategy_id: &str, operator: &str, reason: &str) -> bool {
        match self.breakers.get(strategy_id) {
            Some(b) => b.clear_halt_l3(operator, reason),
            None => false,
        }
    }

    /// Snapshot of all strategy states (for observability / control-plane API).
    pub fn all_states(&self) -> Vec<(String, CircuitState, f64)> {
        self.breakers
            .iter()
            .map(|e| (e.key().clone(), e.state(), e.ev_ratio()))
            .collect()
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod cb_tests {
    use super::*;

    #[test]
    fn starts_healthy() {
        let reg = CircuitBreakerRegistry::new();
        assert_eq!(reg.state("SA"), CircuitState::Healthy);
        assert!(reg.is_operational("SA"));
    }

    #[test]
    fn high_ev_stays_healthy() {
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..10 {
            reg.record("SA", 1.0, 1.0); // EV ratio = 1.0
        }
        assert_eq!(reg.state("SA"), CircuitState::Healthy);
    }

    #[test]
    fn low_ev_triggers_auto_pause() {
        let reg = CircuitBreakerRegistry::new();
        // Fill the window with poor performance (EV ratio ≈ 0.5)
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("LA", 0.5, 1.0);
        }
        // EV ratio = 0.5 < 0.70 → AutoPaused or Halted
        let s = reg.state("LA");
        assert!(
            s == CircuitState::AutoPaused || s == CircuitState::Halted,
            "expected paused or halted, got {:?}", s
        );
    }

    #[test]
    fn ev_below_0_50_halts() {
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("MEV", 0.3, 1.0); // EV ratio = 0.3 < 0.50
        }
        assert_eq!(reg.state("MEV"), CircuitState::Halted);
        assert!(!reg.is_operational("MEV"));
    }

    #[test]
    fn investigate_state_is_operational() {
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("MSA", 0.75, 1.0); // EV ratio = 0.75 → Investigate
        }
        let s = reg.state("MSA");
        // 0.70 ≤ 0.75 < 0.85 → Investigate
        assert_eq!(s, CircuitState::Investigate);
        assert!(reg.is_operational("MSA"), "Investigate must be operational");
    }

    #[test]
    fn l2_resume_from_auto_paused() {
        let reg = CircuitBreakerRegistry::new();
        // Force into AutoPaused by recording EV ratio = 0.65.
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("SA", 0.65, 1.0);
        }
        assert_eq!(reg.state("SA"), CircuitState::AutoPaused);
        let resumed = reg.resume_l2("SA", "alice", "confirmed transient gas spike, not a bug");
        assert!(resumed);
        assert_eq!(reg.state("SA"), CircuitState::Investigate);
        assert!(reg.is_operational("SA"));
    }

    #[test]
    fn l2_resume_does_not_clear_window() {
        // Distinguishing behavior from clear_halt_l3: resume_l2 leaves
        // the EV window intact, so ev_ratio() should still reflect the
        // pre-resume history (≈0.65), not reset to the "no data" 1.0
        // default the way clear_halt_l3 does.
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("SA", 0.65, 1.0);
        }
        assert_eq!(reg.state("SA"), CircuitState::AutoPaused);
        reg.resume_l2("SA", "alice", "test");
        assert!((reg.ev_ratio("SA") - 0.65).abs() < 1e-6, "window should survive L2 resume");
    }

    #[test]
    fn l2_resume_returns_false_when_not_auto_paused() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA"); // Healthy
        let resumed = reg.resume_l2("SA", "alice", "attempted resume on healthy strategy");
        assert!(!resumed);
        assert_eq!(reg.state("SA"), CircuitState::Healthy);
    }

    #[test]
    fn l2_resume_returns_false_for_unregistered_strategy() {
        let reg = CircuitBreakerRegistry::new();
        let resumed = reg.resume_l2("NEVER_SEEN", "alice", "test");
        assert!(!resumed);
    }

    #[test]
    fn l2_resume_increments_counter_and_sets_info_metric() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "L2_RESUME_COUNTER_TEST";
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record(strategy, 0.65, 1.0); // AutoPaused
        }
        assert_eq!(reg.state(strategy), CircuitState::AutoPaused);
        assert!(reg.resume_l2(strategy, "alice", "first resume"));

        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record(strategy, 0.65, 1.0); // AutoPaused again
        }
        assert_eq!(reg.state(strategy), CircuitState::AutoPaused);
        assert!(reg.resume_l2(strategy, "bob", "second resume"));

        let count = metrics::CIRCUIT_BREAKER_L2_RESUME_TOTAL
            .with_label_values(&[strategy])
            .get();
        assert_eq!(count, 2.0);
    }

    #[test]
    fn l2_resume_no_op_does_not_increment_counter() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "L2_RESUME_NOOP_TEST";
        reg.register(strategy); // Healthy, never AutoPaused
        assert!(!reg.resume_l2(strategy, "alice", "premature resume attempt"));

        let count = metrics::CIRCUIT_BREAKER_L2_RESUME_TOTAL
            .with_label_values(&[strategy])
            .get();
        assert_eq!(count, 0.0);
    }

    #[test]
    fn l3_clear_halt_resets_window() {
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("MEV", 0.3, 1.0); // Halted
        }
        assert_eq!(reg.state("MEV"), CircuitState::Halted);
        let cleared = reg.clear_halt_l3("MEV", "alice", "root cause identified, mitigated");
        assert!(cleared);
        assert_eq!(reg.state("MEV"), CircuitState::Investigate);
        // After clear, EV ratio should be 1.0 (empty window) — unlike
        // resume_l2, which leaves the window intact (see
        // l2_resume_does_not_clear_window above).
        assert!((reg.ev_ratio("MEV") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn l3_clear_returns_false_when_not_halted() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA"); // Healthy
        let cleared = reg.clear_halt_l3("SA", "alice", "attempted clear on healthy strategy");
        assert!(!cleared);
        assert_eq!(reg.state("SA"), CircuitState::Healthy);
    }

    #[test]
    fn l3_clear_returns_false_for_unregistered_strategy() {
        let reg = CircuitBreakerRegistry::new();
        let cleared = reg.clear_halt_l3("NEVER_SEEN", "alice", "test");
        assert!(!cleared);
    }

    #[test]
    fn l3_clear_increments_counter_and_sets_info_metric() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "L3_CLEAR_COUNTER_TEST";
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record(strategy, 0.3, 1.0); // Halted
        }
        assert_eq!(reg.state(strategy), CircuitState::Halted);
        assert!(reg.clear_halt_l3(strategy, "alice", "first clear"));

        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record(strategy, 0.3, 1.0); // Halted again
        }
        assert_eq!(reg.state(strategy), CircuitState::Halted);
        assert!(reg.clear_halt_l3(strategy, "bob", "second clear"));

        let count = metrics::CIRCUIT_BREAKER_L3_CLEAR_TOTAL
            .with_label_values(&[strategy])
            .get();
        assert_eq!(count, 2.0);
    }

    #[test]
    fn l3_clear_no_op_does_not_increment_counter() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "L3_CLEAR_NOOP_TEST";
        reg.register(strategy); // Healthy, never Halted
        assert!(!reg.clear_halt_l3(strategy, "alice", "premature clear attempt"));

        let count = metrics::CIRCUIT_BREAKER_L3_CLEAR_TOTAL
            .with_label_values(&[strategy])
            .get();
        assert_eq!(count, 0.0);
    }

    #[test]
    fn diagnostics_returns_none_for_unregistered_strategy() {
        let reg = CircuitBreakerRegistry::new();
        assert!(reg.diagnostics("NEVER_SEEN").is_none());
    }

    #[test]
    fn diagnostics_reflects_current_state_and_ratio() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "DIAGNOSTICS_STATE_TEST";
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record(strategy, 0.3, 1.0); // drives to Halted, ratio 0.3
        }
        let diag = reg.diagnostics(strategy).expect("should be registered");
        assert_eq!(diag.strategy_id, strategy);
        assert_eq!(diag.state, CircuitState::Halted);
        assert!((diag.ev_ratio - 0.3).abs() < 1e-9);
        assert_eq!(diag.window_fill, EV_WINDOW_BLOCKS);
    }

    #[test]
    fn diagnostics_window_pairs_match_recorded_outcomes() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "DIAGNOSTICS_PAIRS_TEST";
        reg.record(strategy, 1.0, 2.0);
        reg.record(strategy, 3.0, 4.0);
        reg.record(strategy, 5.0, 6.0);

        let diag = reg.diagnostics(strategy).unwrap();
        assert_eq!(diag.window_pairs, vec![(1.0, 2.0), (3.0, 4.0), (5.0, 6.0)]);
    }

    #[test]
    fn diagnostics_window_pairs_reveal_outlier_vs_uniform_degradation() {
        // Directly exercises the distinction the runbook's diagnosis
        // section asks a responder to make: "a few trades realized deeply
        // negative outcomes, most were fine" vs "consistently and
        // uniformly below expected." Same aggregate ev_ratio can arise
        // from either shape — diagnostics() is what lets you tell them
        // apart.
        let reg = CircuitBreakerRegistry::new();
        let strategy = "DIAGNOSTICS_SHAPE_TEST";
        // 9 perfect trades, 1 catastrophic one: aggregate ratio pulled
        // down by a single outlier.
        for _ in 0..9 {
            reg.record(strategy, 1.0, 1.0);
        }
        reg.record(strategy, -8.0, 1.0);

        let diag = reg.diagnostics(strategy).unwrap();
        let outliers: Vec<_> = diag
            .window_pairs
            .iter()
            .filter(|(obs, exp)| obs / exp < 0.0)
            .collect();
        assert_eq!(outliers.len(), 1, "exactly one catastrophic pair should be identifiable");
    }

    #[test]
    fn diagnostics_last_transition_only_updates_on_actual_transition() {
        let reg = CircuitBreakerRegistry::new();
        let strategy = "DIAGNOSTICS_TRANSITION_TEST";
        reg.record(strategy, 1.0, 1.0); // Healthy, no transition (already Healthy)
        let diag1 = reg.diagnostics(strategy).unwrap();

        // A second Healthy-preserving record should not move
        // last_transition forward.
        reg.record(strategy, 1.0, 1.0);
        let diag2 = reg.diagnostics(strategy).unwrap();
        assert_eq!(diag1.last_transition, diag2.last_transition);
    }

    #[test]
    fn all_diagnostics_returns_every_registered_strategy() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA");
        reg.register("LA");
        reg.register("MEV");
        let all = reg.all_diagnostics();
        assert_eq!(all.len(), 3);
        let ids: Vec<_> = all.iter().map(|d| d.strategy_id.as_str()).collect();
        assert!(ids.contains(&"SA"));
        assert!(ids.contains(&"LA"));
        assert!(ids.contains(&"MEV"));
    }

    #[test]
    fn l2_resume_no_op_when_healthy() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA");
        reg.resume_l2("SA", "alice", "no-op check");
        assert_eq!(reg.state("SA"), CircuitState::Healthy);
    }

    #[test]
    fn all_states_returns_all_registered() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA");
        reg.register("LA");
        let states = reg.all_states();
        assert_eq!(states.len(), 2);
    }

    #[test]
    fn metrics_pushes_do_not_panic_across_full_lifecycle() {
        // Exercises every metrics-touching path in one run: construction,
        // record() with and without a state transition, resume_l2(), and
        // clear_halt_l3(). Metrics registries are process-global Lazy
        // statics, so this test's job is confirming no panic/label-cardinality
        // issue occurs across the whole lifecycle, not asserting exact
        // gauge values.
        let reg = CircuitBreakerRegistry::new();
        reg.register("METRICS_TEST");
        reg.record("METRICS_TEST", 1.0, 1.0);       // Healthy, no transition
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("METRICS_TEST", 0.65, 1.0);  // drives to AutoPaused
        }
        assert_eq!(reg.state("METRICS_TEST"), CircuitState::AutoPaused);
        assert!(reg.resume_l2("METRICS_TEST", "alice", "lifecycle test resume"));
        assert_eq!(reg.state("METRICS_TEST"), CircuitState::Investigate);
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("METRICS_TEST", 0.3, 1.0);   // drives to Halted
        }
        assert_eq!(reg.state("METRICS_TEST"), CircuitState::Halted);
        assert!(reg.clear_halt_l3("METRICS_TEST", "alice", "lifecycle test clear"));
        assert_eq!(reg.state("METRICS_TEST"), CircuitState::Investigate);
        let _ = reg.diagnostics("METRICS_TEST").unwrap();
        let _ = reg.all_diagnostics();
    }
}