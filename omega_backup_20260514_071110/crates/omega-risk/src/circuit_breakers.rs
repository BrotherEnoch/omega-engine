// crates/omega-risk/src/circuit_breakers.rs
//
// Per-strategy EV-ratio circuit breakers (spec Section 19 â€” Adaptive EV-Weighted Rollout).
//
// EV ratio = observed_profit / expected_profit over a 72-block rolling window.
//
// Thresholds (spec S19 / adverse selection detector S8):
//   â‰¥ 0.85              â†’ Healthy â€” no action.
//   0.70â€“0.85           â†’ Investigate â€” emit alert, reduce rollout tier.
//   0.50â€“0.70 for N blocks â†’ AutoPaused â€” L2 fast-approve required to resume.
//   < 0.50              â†’ Halted â€” L3 governance required (circuit break).
//
// Implementation:
//   â€¢ One `StrategyCircuitBreaker` per strategy ID, stored in a DashMap.
//   â€¢ Each breaker holds a VecDeque window of (observed, expected) pairs.
//   â€¢ State transitions are lock-free for reads (AtomicU8) and use a Mutex
//     only for the window write path.
//   â€¢ `CircuitBreakerRegistry` is the shared handle injected into all strategy tasks.
//
// Spec: "< 0.70 for 72 blocks â†’ AUTO-PAUSED (L2 fast-approve to resume)"
//       "< 0.50 â†’ circuit-break (L3 governance required)"

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use chrono::{DateTime, Utc};

// â”€â”€â”€ Spec thresholds â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Rolling window size in blocks (spec S19: 72 blocks).
pub const EV_WINDOW_BLOCKS: usize = 72;

/// EV ratio below which strategy enters Investigate state (spec S19: 0.85).
pub const EV_INVESTIGATE_THRESHOLD: f64 = 0.85;

/// EV ratio below which strategy is AUTO-PAUSED (spec S19: 0.70).
pub const EV_AUTO_PAUSE_THRESHOLD: f64 = 0.70;

/// EV ratio below which strategy is circuit-broken / Halted (spec S19: 0.50).
pub const EV_HALT_THRESHOLD: f64 = 0.50;

// â”€â”€â”€ Circuit state FSM â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-strategy circuit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// EV ratio â‰¥ 0.85 â€” operating normally.
    Healthy,
    /// 0.70 â‰¤ EV ratio < 0.85 â€” emit alert, reduce rollout, continue.
    Investigate,
    /// EV ratio < 0.70 sustained over window â€” paused; L2 fast-approve to resume.
    AutoPaused,
    /// EV ratio < 0.50 â€” hard stop; L3 governance required.
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

// â”€â”€â”€ Per-strategy breaker â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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
        Self {
            strategy_id:     strategy_id.into(),
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

    /// Current EV ratio (requires window lock â€” use sparingly in hot path).
    pub fn ev_ratio(&self) -> f64 {
        self.window.lock().ev_ratio()
    }

    /// Window fill level (0..=EV_WINDOW_BLOCKS).
    pub fn window_size(&self) -> usize {
        self.window.lock().len()
    }

    // â”€â”€ Governance recovery actions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Resume from AutoPaused (L2 fast-approve path, spec S19).
    /// Resets state to Investigate (not immediately Healthy) to stay cautious.
    pub fn resume_l2(&self) {
        let current = self.state();
        if current == CircuitState::AutoPaused {
            self.state.store(CircuitState::Investigate.to_u8(), Ordering::Release);
            *self.last_transition.lock() = Utc::now();
            tracing::info!(strategy = %self.strategy_id, "circuit breaker resumed (L2)");
        }
    }

    /// Clear from Halted (L3 governance path, spec S19).
    /// Resets state to Investigate and clears the EV window.
    pub fn clear_halt_l3(&self) {
        let current = self.state();
        if current == CircuitState::Halted {
            // Clear the window so the EV ratio starts fresh.
            {
                let mut win = self.window.lock();
                win.deque.clear();
            }
            self.state.store(CircuitState::Investigate.to_u8(), Ordering::Release);
            *self.last_transition.lock() = Utc::now();
            tracing::warn!(strategy = %self.strategy_id, "circuit breaker cleared (L3 governance)");
        }
    }
}

// â”€â”€â”€ Registry â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

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

    /// Resume an AutoPaused strategy via L2 governance.
    pub fn resume_l2(&self, strategy_id: &str) {
        if let Some(b) = self.breakers.get(strategy_id) {
            b.resume_l2();
        }
    }

    /// Clear a Halted strategy via L3 governance.
    pub fn clear_halt_l3(&self, strategy_id: &str) {
        if let Some(b) = self.breakers.get(strategy_id) {
            b.clear_halt_l3();
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
        // Fill the window with poor performance (EV ratio â‰ˆ 0.5)
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("LA", 0.5, 1.0);
        }
        // EV ratio = 0.5 < 0.70 â†’ AutoPaused or Halted
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
            reg.record("MSA", 0.75, 1.0); // EV ratio = 0.75 â†’ Investigate
        }
        let s = reg.state("MSA");
        // 0.70 â‰¤ 0.75 < 0.85 â†’ Investigate
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
        reg.resume_l2("SA");
        assert_eq!(reg.state("SA"), CircuitState::Investigate);
        assert!(reg.is_operational("SA"));
    }

    #[test]
    fn l3_clear_halt_resets_window() {
        let reg = CircuitBreakerRegistry::new();
        for _ in 0..EV_WINDOW_BLOCKS {
            reg.record("MEV", 0.3, 1.0); // Halted
        }
        assert_eq!(reg.state("MEV"), CircuitState::Halted);
        reg.clear_halt_l3("MEV");
        assert_eq!(reg.state("MEV"), CircuitState::Investigate);
        // After clear, EV ratio should be 1.0 (empty window).
        assert!((reg.ev_ratio("MEV") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn l2_resume_no_op_when_healthy() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("SA");
        reg.resume_l2("SA");
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
}