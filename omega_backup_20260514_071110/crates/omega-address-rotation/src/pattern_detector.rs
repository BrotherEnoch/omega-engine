// crates/omega-address-rotation/src/pattern_detector.rs
//
// Pattern detector â€” rolling-window `LOST_RACE_SAME_FEE` fingerprint guard
// (spec Â§14).
//
// ## Purpose
//
//   When block builders or relay operators fingerprint our execution
//   address, they can preferentially order competing bundles ahead of
//   ours even when both bundles submit the same priority fee.  The
//   observable symptom is an elevated `LOST_RACE_SAME_FEE` loss rate.
//
//   The pattern detector watches the rolling 200-event window and emits
//   a rotation trigger when the `LOST_RACE_SAME_FEE` fraction exceeds
//   the configured threshold (default 20%).
//
// ## Window model
//
//   A bounded `VecDeque<bool>` is maintained where:
//     `true`  = this loss event was `LOST_RACE_SAME_FEE`
//     `false` = any other loss code
//
//   Window capacity = `PatternDetectorConfig::window_size` (default 200).
//   Old entries are evicted as new ones arrive (FIFO).
//
// ## Spec Â§14 triggers
//
//   Primary trigger: `same_fee_rate > threshold` (default 0.20 = 20%).
//
//   Secondary trigger enforced by `AddressRotationManager` separately:
//   30-day schedule regardless of pattern.
//
// ## Thread safety
//
//   `PatternDetector` is NOT `Send + Sync` â€” it is owned exclusively by
//   the rotation manager task, which processes loss events sequentially.
//   If concurrent access is needed, wrap in `Mutex<PatternDetector>`.

use std::collections::VecDeque;

use omega_loss_attribution::LossCode;
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PatternDetectorConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Configuration for the fingerprinting pattern detector (Â§14).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternDetectorConfig {
    /// Number of loss events in the rolling window.
    ///
    /// Default 200 â€” provides enough statistical signal to distinguish
    /// a genuine fingerprinting event from random same-fee races.
    pub window_size: usize,

    /// `LOST_RACE_SAME_FEE` fraction above which rotation is triggered.
    ///
    /// Default 0.20 = 20% (spec Â§14).
    pub trigger_threshold: f64,

    /// Minimum window occupancy before the detector can trigger.
    ///
    /// Prevents false positives early in a rotation cycle when the window
    /// has few events.  Default 50 events = 25% of window.
    pub min_events_before_trigger: usize,
}

impl Default for PatternDetectorConfig {
    fn default() -> Self {
        Self {
            window_size:               200,
            trigger_threshold:         0.20,
            min_events_before_trigger: 50,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PatternDetector
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Rolling-window `LOST_RACE_SAME_FEE` fingerprint detector (Â§14).
///
/// Not `Send + Sync` â€” owned by the rotation manager task only.
pub struct PatternDetector {
    config:        PatternDetectorConfig,
    /// Rolling window: `true` = LOST_RACE_SAME_FEE, `false` = other.
    window:        VecDeque<bool>,
    /// Cached count of `true` entries for O(1) rate computation.
    same_fee_count: usize,
}

impl PatternDetector {
    /// Create a new detector with the given configuration.
    pub fn new(config: PatternDetectorConfig) -> Self {
        let cap = config.window_size;
        Self {
            config,
            window:         VecDeque::with_capacity(cap),
            same_fee_count: 0,
        }
    }

    /// Create with default configuration.
    pub fn default_config() -> Self {
        Self::new(PatternDetectorConfig::default())
    }

    // â”€â”€ Recording â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Record one loss event.
    ///
    /// Returns `true` when the event causes the fingerprinting threshold
    /// to be crossed â€” the caller should trigger an address rotation.
    pub fn record(&mut self, code: LossCode) -> bool {
        let is_same_fee = code == LossCode::LostRaceSameFee;

        // Evict oldest entry if window is full
        if self.window.len() == self.config.window_size {
            if let Some(old) = self.window.pop_front() {
                if old { self.same_fee_count -= 1; }
            }
        }

        self.window.push_back(is_same_fee);
        if is_same_fee { self.same_fee_count += 1; }

        self.should_trigger()
    }

    /// Current `LOST_RACE_SAME_FEE` fraction in the rolling window.
    ///
    /// Returns 0.0 when the window is empty.
    pub fn same_fee_rate(&self) -> f64 {
        let n = self.window.len();
        if n == 0 { return 0.0; }
        self.same_fee_count as f64 / n as f64
    }

    /// Returns `true` when the window has enough events and the
    /// `LOST_RACE_SAME_FEE` rate exceeds the configured threshold.
    pub fn should_trigger(&self) -> bool {
        self.window.len() >= self.config.min_events_before_trigger
            && self.same_fee_rate() > self.config.trigger_threshold
    }

    /// Number of events currently in the rolling window.
    pub fn window_len(&self) -> usize {
        self.window.len()
    }

    /// Reset the window â€” called after a rotation so the detector starts
    /// fresh for the new address.
    pub fn reset(&mut self) {
        self.window.clear();
        self.same_fee_count = 0;
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> PatternDetector {
        PatternDetector::new(PatternDetectorConfig {
            window_size:               10,
            trigger_threshold:         0.20,
            min_events_before_trigger: 5,
        })
    }

    #[test]
    fn empty_window_no_trigger() {
        let d = detector();
        assert!(!d.should_trigger());
        assert_eq!(d.same_fee_rate(), 0.0);
    }

    #[test]
    fn below_threshold_no_trigger() {
        let mut d = detector();
        // 1 same-fee in 10 events = 10% < 20% threshold
        for _ in 0..9  { d.record(LossCode::LostGasLow); }
        d.record(LossCode::LostRaceSameFee);
        assert!(!d.should_trigger());
    }

    #[test]
    fn above_threshold_triggers() {
        let mut d = detector();
        // 5 same-fee in 10 events = 50% > 20%
        for _ in 0..5 { d.record(LossCode::LostRaceSameFee); }
        for _ in 0..5 { d.record(LossCode::LostGasLow); }
        assert!(d.should_trigger());
    }

    #[test]
    fn trigger_requires_min_events() {
        let mut d = detector(); // min_events = 5
        // 3 same-fee in 4 events = 75%, but < min_events â†’ no trigger
        for _ in 0..4 { d.record(LossCode::LostRaceSameFee); }
        assert!(!d.should_trigger(),
            "must not trigger before min_events_before_trigger");
    }

    #[test]
    fn window_evicts_oldest() {
        let mut d = detector(); // window_size = 10
        // Fill with same-fee â†’ rate = 100%
        for _ in 0..10 { d.record(LossCode::LostRaceSameFee); }
        assert!(d.same_fee_rate() > 0.99);

        // Push 10 non-same-fee events â€” should evict all same-fee entries
        for _ in 0..10 { d.record(LossCode::LostGasLow); }
        assert_eq!(d.same_fee_count, 0);
        assert!((d.same_fee_rate() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn record_returns_trigger_status() {
        let mut d = detector();
        for _ in 0..4 { d.record(LossCode::LostGasLow); }
        // 5th event, all different â€” no trigger
        let triggered = d.record(LossCode::LostRaceSameFee);
        // 1/5 = 20% â€” threshold is STRICTLY greater than, so 20% == 20% â†’ no trigger
        assert!(!triggered);

        // Push enough same-fee to cross threshold (>20%)
        let triggered2 = d.record(LossCode::LostRaceSameFee);
        // 2/6 = 33% > 20% â†’ trigger
        assert!(triggered2);
    }

    #[test]
    fn reset_clears_state() {
        let mut d = detector();
        for _ in 0..8 { d.record(LossCode::LostRaceSameFee); }
        assert!(d.should_trigger());
        d.reset();
        assert!(!d.should_trigger());
        assert_eq!(d.window_len(), 0);
        assert_eq!(d.same_fee_count, 0);
    }

    #[test]
    fn non_same_fee_codes_do_not_increment() {
        let mut d = detector();
        for code in [
            LossCode::LostGasLow,
            LossCode::LostGasOverbid,
            LossCode::LostLatency,
            LossCode::MissedDetection,
            LossCode::SimulationGasMiscalc,
        ] {
            d.record(code);
        }
        assert_eq!(d.same_fee_count, 0);
    }
}