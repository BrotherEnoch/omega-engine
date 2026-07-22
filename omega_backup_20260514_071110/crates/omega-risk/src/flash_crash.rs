// crates/omega-risk/src/flash_crash.rs
//
// Graduated Flash-Crash Guard (spec Section 11 LA â€” S11.4).
//
// Replaces v11's full-pause approach with a graduated response that keeps LA
// active during the highest-EV windows (imminent liquidations at HF < 1.001).
//
// Trigger conditions (spec S11):
//   Spike: price moves >10 % in 5 blocks.
//   Drift: cumulative move >15 % over 20 blocks.
//
// Response when triggered (graduated, not full-pause):
//   â€¢ Reduce maximum liquidation size to 50 % of normal.
//   â€¢ Raise minimum profit threshold multiplier from 1.5Ã— to 2.5Ã— gas.
//   â€¢ Tighten oracle agreement from 0.4 % to 0.1 %.
//
// Exception: at HF < 1.001 during a spike, max priority fee is used (never
// pause during the maximum-EV window â€” spec note in S11).
//
// Thread-safety:
//   `Arc<FlashCrashGuard>` is shared across oracle-update callbacks.
//   Uses a Mutex<VecDeque<f64>> for the price window (only written on oracle updates,
//   not in the hot scoring path).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// â”€â”€â”€ Spec constants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Spike detection window in blocks (spec: 5 blocks).
pub const SPIKE_WINDOW_BLOCKS:    usize = 5;
/// Drift detection window in blocks (spec: 20 blocks).
pub const DRIFT_WINDOW_BLOCKS:    usize = 20;
/// Spike price move threshold (spec: >10 %).
pub const SPIKE_THRESHOLD:        f64 = 0.10;
/// Drift price move threshold (spec: >15 %).
pub const DRIFT_THRESHOLD:        f64 = 0.15;

/// Maximum liquidation size during flash-crash response (50 % of normal).
pub const FLASH_CRASH_MAX_SIZE_PCT: f64 = 0.50;
/// Minimum profit multiplier during flash-crash response (2.5Ã— gas).
pub const FLASH_CRASH_MIN_PROFIT_MULT: f64 = 2.50;
/// Tightened oracle agreement during flash-crash (0.1 %).
pub const FLASH_CRASH_ORACLE_AGREEMENT_PCT: f64 = 0.001;

// â”€â”€â”€ Flash crash response types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Graduated flash-crash response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FlashCrashResponse {
    /// Normal market conditions â€” no adjustments.
    Normal,
    /// Flash-crash detected â€” apply graduated adjustments.
    Graduated {
        /// Maximum liquidation size as a fraction of the blueprint amount (e.g., 0.50).
        max_size_fraction:    f64,
        /// Minimum profit multiplier applied on top of the normal dynamic_min_profit.
        min_profit_mult:      f64,
        /// Tightened oracle price-agreement threshold (e.g., 0.001 = 0.1 %).
        oracle_agreement_pct: f64,
        /// True when HF < 1.001 â€” max priority fee overrides the response.
        imminent_liquidation: bool,
    },
}

impl FlashCrashResponse {
    pub fn is_normal(&self) -> bool {
        matches!(self, FlashCrashResponse::Normal)
    }

    pub fn is_graduated(&self) -> bool {
        matches!(self, FlashCrashResponse::Graduated { .. })
    }

    /// True when flash crash is active AND the position is NOT in the imminent window.
    /// In the imminent window (HF < 1.001), we proceed at max priority fee regardless.
    pub fn should_reduce_size(&self) -> bool {
        matches!(self, FlashCrashResponse::Graduated { imminent_liquidation: false, .. })
    }
}

// â”€â”€â”€ Guard implementation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Per-asset flash-crash guard.
///
/// Oracle-update callbacks call `push_price()`.
/// Scoring path calls `evaluate(health_factor)` to get the current response.
#[derive(Debug)]
pub struct FlashCrashGuard {
    // We store the last DRIFT_WINDOW_BLOCKS prices (drift window is larger).
    history: Mutex<VecDeque<f64>>,
}

impl FlashCrashGuard {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(VecDeque::with_capacity(DRIFT_WINDOW_BLOCKS + 1)),
        }
    }

    /// Push a new price sample from the oracle (called on every oracle update).
    pub fn push_price(&self, price: f64) {
        let mut h = self.history.lock();
        // Keep last DRIFT_WINDOW_BLOCKS samples.
        if h.len() >= DRIFT_WINDOW_BLOCKS {
            h.pop_front();
        }
        h.push_back(price);
    }

    /// Evaluate current market conditions.
    ///
    /// Returns `Normal` or `Graduated` based on spike/drift detection.
    pub fn evaluate(&self, health_factor: f64) -> FlashCrashResponse {
        let snapshot: Vec<f64> = {
            let h = self.history.lock();
            h.iter().copied().collect()
        };

        let spike = self.detect_spike(&snapshot);
        let drift = self.detect_drift(&snapshot);

        if !spike && !drift {
            return FlashCrashResponse::Normal;
        }

        let imminent = health_factor < 1.001;

        FlashCrashResponse::Graduated {
            max_size_fraction:    FLASH_CRASH_MAX_SIZE_PCT,
            min_profit_mult:      FLASH_CRASH_MIN_PROFIT_MULT,
            oracle_agreement_pct: FLASH_CRASH_ORACLE_AGREEMENT_PCT,
            imminent_liquidation: imminent,
        }
    }

    /// True if price has moved >10 % in the last 5 blocks (spec S11).
    fn detect_spike(&self, prices: &[f64]) -> bool {
        if prices.len() < SPIKE_WINDOW_BLOCKS {
            return false;
        }
        let current = *prices.last().unwrap();
        let baseline = prices[prices.len() - SPIKE_WINDOW_BLOCKS];
        if baseline <= 0.0 { return false; }
        let move_pct = (current - baseline).abs() / baseline;
        move_pct > SPIKE_THRESHOLD
    }

    /// True if cumulative price move exceeds 15 % over the full 20-block window (spec S11).
    fn detect_drift(&self, prices: &[f64]) -> bool {
        if prices.len() < DRIFT_WINDOW_BLOCKS {
            return false;
        }
        let oldest  = prices[0];
        let current = *prices.last().unwrap();
        if oldest <= 0.0 { return false; }
        let move_pct = (current - oldest).abs() / oldest;
        move_pct > DRIFT_THRESHOLD
    }

    /// Current price history length.
    pub fn history_len(&self) -> usize {
        self.history.lock().len()
    }
}

impl Default for FlashCrashGuard {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod flash_crash_tests {
    use super::*;

    fn make_flat_guard(price: f64, n: usize) -> FlashCrashGuard {
        let g = FlashCrashGuard::new();
        for _ in 0..n {
            g.push_price(price);
        }
        g
    }

    // â”€â”€ Normal conditions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn flat_market_is_normal() {
        let g = make_flat_guard(2000.0, 20);
        assert!(g.evaluate(1.05).is_normal());
    }

    #[test]
    fn insufficient_history_is_normal() {
        let g = FlashCrashGuard::new();
        g.push_price(2000.0);
        assert!(g.evaluate(1.05).is_normal());
    }

    // â”€â”€ Spike detection â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn spike_above_10pct_in_5_blocks_triggers_graduated() {
        let g = FlashCrashGuard::new();
        // Stable prices
        for _ in 0..15 { g.push_price(2000.0); }
        // Sudden 15 % drop in last sample
        g.push_price(1700.0);
        assert!(g.evaluate(1.05).is_graduated());
    }

    #[test]
    fn small_move_below_10pct_stays_normal() {
        let g = FlashCrashGuard::new();
        for _ in 0..19 { g.push_price(2000.0); }
        g.push_price(2050.0); // 2.5 % â€” below 10 %
        assert!(g.evaluate(1.05).is_normal());
    }

    // â”€â”€ Drift detection â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn drift_above_15pct_over_20_blocks_triggers_graduated() {
        let g = FlashCrashGuard::new();
        // Gradual drift: start 2000, end 2320 â†’ (2320-2000)/2000 = 16 % > 15 %
        for i in 0..20 {
            g.push_price(2000.0 + i as f64 * 16.0);
        }
        assert!(g.evaluate(1.05).is_graduated());
    }

    #[test]
    fn drift_below_15pct_stays_normal() {
        let g = FlashCrashGuard::new();
        // Gradual drift: start 2000, end 2240 â†’ 12 % < 15 %
        for i in 0..20 {
            g.push_price(2000.0 + i as f64 * 12.0);
        }
        assert!(g.evaluate(1.05).is_normal());
    }

    // â”€â”€ Graduated response fields â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn graduated_response_has_correct_values() {
        let g = FlashCrashGuard::new();
        for _ in 0..19 { g.push_price(2000.0); }
        g.push_price(1700.0); // spike
        match g.evaluate(1.05) {
            FlashCrashResponse::Graduated { max_size_fraction, min_profit_mult, oracle_agreement_pct, .. } => {
                assert!((max_size_fraction - 0.50).abs() < 1e-9);
                assert!((min_profit_mult - 2.50).abs() < 1e-9);
                assert!((oracle_agreement_pct - 0.001).abs() < 1e-9);
            }
            _ => panic!("expected Graduated"),
        }
    }

    #[test]
    fn imminent_liquidation_flag_set_when_hf_below_1_001() {
        let g = FlashCrashGuard::new();
        for _ in 0..19 { g.push_price(2000.0); }
        g.push_price(1700.0);
        match g.evaluate(1.0005) {
            FlashCrashResponse::Graduated { imminent_liquidation: true, .. } => {}
            other => panic!("expected imminent=true, got {:?}", other),
        }
    }

    #[test]
    fn non_imminent_hf_does_not_set_imminent_flag() {
        let g = FlashCrashGuard::new();
        for _ in 0..19 { g.push_price(2000.0); }
        g.push_price(1700.0);
        match g.evaluate(1.05) {
            FlashCrashResponse::Graduated { imminent_liquidation: false, .. } => {}
            other => panic!("expected imminent=false, got {:?}", other),
        }
    }

    #[test]
    fn rolling_window_evicts_oldest_prices() {
        let g = FlashCrashGuard::new();
        // Fill 20 blocks of stable prices.
        for _ in 0..20 { g.push_price(2000.0); }
        assert_eq!(g.history_len(), 20);
        // Push one more â€” should evict oldest (still 20).
        g.push_price(2010.0);
        assert_eq!(g.history_len(), 20);
    }
}