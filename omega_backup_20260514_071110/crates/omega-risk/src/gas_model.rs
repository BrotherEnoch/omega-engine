ï»¿// crates/omega-risk/src/gas_model.rs
//
// Arbitrum Dual-Component Gas Model (spec Section 7).
//
// Arbitrum has two independent fee components:
//
//   1. L2 execution gas  â€” stable; near-constant base fee (0.01â€“0.1 gwei).
//      Buffer: fixed 1.10Ã— (spec: L2_EXEC_BUFFER).
//
//   2. L1 data cost      â€” volatile; tracks Ethereum L1 gas price.
//      Buffer: EMA-adaptive 1.30Ã—â€“2.00Ã— (spec: l1_adaptive_buffer).
//      Computed from coefficient of variation of the rolling price history.
//
//   3. Extraction gas    â€” fixed 45,000 units for vault.receivePendingProfit()
//      (spec: "extraction_gas = 45_000").
//
// Priority fee (spec S12 / Arbitrum note I3):
//   On Arbitrum the sequencer prioritises by tip. Base fee near-constant so
//   500 gwei ceiling is appropriate â€” ~0.0105 ETH per block at 0.25 s blocks.
//
// EMA adaptive buffer:
//   Uses an exponential moving average to track the L1 price trend so the
//   buffer widens during high-volatility windows and narrows when L1 is calm.
//   Coefficient of variation (Ïƒ/Î¼) drives the multiplier: 1.30 + CV Ã— 3.50,
//   clamped to [1.30, 2.00].
//
// Thread-safety:
//   All functions are pure / stateless.  The caller (omega-strategies) holds
//   an Arc<L1GasEma> for the rolling EMA state.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// â”€â”€â”€ Spec constants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Fixed gas units for vault.receivePendingProfit() (spec: extraction_gas = 45_000).
pub const EXTRACTION_GAS: u64 = 45_000;

/// Fixed L2 execution buffer factor (spec: L2_EXEC_BUFFER = 1.10).
pub const L2_EXEC_BUFFER: f64 = 1.10;

/// Minimum L1 adaptive buffer (spec: l1_data_buffer_min = 1.30).
pub const L1_BUFFER_MIN: f64 = 1.30;

/// Maximum L1 adaptive buffer (spec: l1_data_buffer_max = 2.00).
pub const L1_BUFFER_MAX: f64 = 2.00;

/// Scalar for CV â†’ buffer mapping: 1.30 + CV Ã— 3.50.
const L1_CV_SCALAR: f64 = 3.50;

/// Arbitrum priority fee ceiling in gwei (spec S12 / I3: 500 gwei).
pub const MAX_PRIORITY_FEE_GWEI: u64 = 500;

/// Default EMA window for L1 gas price history (spec: l1_ema_window = 20).
pub const L1_EMA_WINDOW: usize = 20;

// â”€â”€â”€ Core gas model functions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compute dynamic minimum profit in gwei.
///
/// Ensures the blueprint will be profitable after paying:
///   - L2 execution cost (stable, 1.10Ã— buffer).
///   - L1 data cost (volatile, adaptive buffer).
///   - Extraction gas for vault.receivePendingProfit().
///
/// Returns `base_min.max(computed_cost)` â€” the caller's floor is always respected.
///
/// # Arguments
/// * `base_min`             â€” strategy base minimum profit floor (gwei)
/// * `l2_exec_gas`          â€” estimated L2 execution gas units
/// * `l1_data_gas`          â€” estimated L1 data gas units
/// * `current_l2_base_fee`  â€” current Arbitrum base fee (gwei, typically 0.01â€“0.1)
/// * `current_l1_gas_price` â€” current Ethereum L1 gas price (gwei, volatile)
/// * `l1_adaptive_buf`      â€” output of `l1_adaptive_buffer()`, clamped [1.30, 2.00]
pub fn dynamic_min_profit(
    base_min:             u64,
    l2_exec_gas:          u64,
    l1_data_gas:          u64,
    current_l2_base_fee:  u64,
    current_l1_gas_price: u64,
    l1_adaptive_buf:      f64,
) -> u64 {
    // L2 execution cost.
    let l2_cost = (l2_exec_gas as f64 * current_l2_base_fee as f64 * L2_EXEC_BUFFER) as u64;

    // L1 data cost with adaptive buffer.
    let l1_cost = (l1_data_gas as f64 * current_l1_gas_price as f64 * l1_adaptive_buf) as u64;

    // Vault extraction gas at L2 rate.
    let ext_cost = (EXTRACTION_GAS as f64 * current_l2_base_fee as f64 * L2_EXEC_BUFFER) as u64;

    base_min.max(l2_cost + l1_cost + ext_cost)
}

/// Compute the L1 adaptive buffer from a rolling price history (spec Section 7).
///
/// Algorithm:
///   1. Compute coefficient of variation: CV = std_dev / last_price.
///   2. buffer = clamp(1.30 + CV Ã— 3.50, 1.30, 2.00).
///
/// Falls back to L1_BUFFER_MIN if history is empty or last price is zero.
pub fn l1_adaptive_buffer(l1_price_history: &[u64]) -> f64 {
    if l1_price_history.is_empty() {
        return L1_BUFFER_MIN;
    }

    let last = match l1_price_history.last() {
        Some(&v) if v > 0 => v as f64,
        _ => return L1_BUFFER_MIN,
    };

    let n = l1_price_history.len() as f64;
    let mean = l1_price_history.iter().map(|&x| x as f64).sum::<f64>() / n;

    let variance = l1_price_history
        .iter()
        .map(|&x| {
            let d = x as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n;

    let std_dev = variance.sqrt();
    let cv      = std_dev / last.max(1.0);

    (L1_BUFFER_MIN + cv * L1_CV_SCALAR).clamp(L1_BUFFER_MIN, L1_BUFFER_MAX)
}

// â”€â”€â”€ Rolling EMA state â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Rolling L1 gas price tracker used by the live strategy pipeline.
///
/// Thread-safe: `Arc<L1GasEma>` is shared across oracle-update callbacks.
/// Uses a VecDeque window (not a full vec copy) so push_price() is O(1).
#[derive(Debug)]
pub struct L1GasEma {
    inner: Mutex<L1GasEmaInner>,
}

#[derive(Debug, Serialize, Deserialize)]
struct L1GasEmaInner {
    history:     VecDeque<u64>,
    window_size: usize,
}

impl L1GasEma {
    pub fn new(window_size: usize) -> Self {
        Self {
            inner: Mutex::new(L1GasEmaInner {
                history:     VecDeque::with_capacity(window_size),
                window_size: window_size.max(1),
            }),
        }
    }

    /// Push a new L1 gas price sample (called on every oracle tick).
    pub fn push_price(&self, gwei: u64) {
        let mut g = self.inner.lock();
        if g.history.len() >= g.window_size {
            g.history.pop_front();
        }
        g.history.push_back(gwei);
    }

    /// Return the current adaptive buffer (snapshot of window).
    pub fn current_buffer(&self) -> f64 {
        let g = self.inner.lock();
        let slice: Vec<u64> = g.history.iter().copied().collect();
        drop(g);
        l1_adaptive_buffer(&slice)
    }

    /// Return the most recent L1 gas price sample, or 0 if no data.
    pub fn latest_gwei(&self) -> u64 {
        let g = self.inner.lock();
        g.history.back().copied().unwrap_or(0)
    }

    /// Return a snapshot copy of the history (used in tests / logging).
    pub fn history_snapshot(&self) -> Vec<u64> {
        let g = self.inner.lock();
        g.history.iter().copied().collect()
    }

    /// Current window length.
    pub fn len(&self) -> usize {
        self.inner.lock().history.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().history.is_empty()
    }
}

// â”€â”€â”€ Gas cost estimation helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Estimate the total gas cost (in gwei) for a bundle at a given fee cap.
///
/// Used by the emergency-bundle profit check (spec S12.1 M2):
///   `emergency_gas_cost = fee_cap_gwei Ã— gas_estimate`
pub fn estimate_gas_cost_gwei(fee_cap_gwei: u64, gas_estimate: u64) -> u64 {
    fee_cap_gwei.saturating_mul(gas_estimate)
}

/// Convert a gwei cost to ETH (18 decimal places).
pub fn gwei_to_eth(gwei: u64) -> f64 {
    gwei as f64 / 1_000_000_000.0
}

/// Compute the conservative, aggressive, and emergency fee caps (spec S12.1).
///
/// Returns `(conservative_gwei, aggressive_gwei, emergency_gwei)`.
pub fn fee_cap_variants(base_cap_gwei: u64) -> (u64, u64, u64) {
    let conservative = (base_cap_gwei as f64 * 0.7) as u64;
    let aggressive   = base_cap_gwei;
    let emergency    = base_cap_gwei.saturating_mul(2);
    (conservative, aggressive, emergency)
}

/// Determine whether the emergency bundle is profitable at 2Ã— fee cap (spec M2).
///
/// Returns `true` if `expected_profit_net > emergency_gas_cost + dynamic_min_profit`.
pub fn emergency_bundle_profitable(
    expected_profit_net: u64,
    emergency_fee_gwei:  u64,
    gas_estimate:        u64,
    dynamic_min_profit:  u64,
) -> bool {
    let emergency_gas_cost = estimate_gas_cost_gwei(emergency_fee_gwei, gas_estimate);
    expected_profit_net > emergency_gas_cost.saturating_add(dynamic_min_profit)
}

#[cfg(test)]
mod gas_model_tests {
    use super::*;

    // â”€â”€ l1_adaptive_buffer â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn empty_history_returns_min_buffer() {
        assert!((l1_adaptive_buffer(&[]) - L1_BUFFER_MIN).abs() < 1e-9);
    }

    #[test]
    fn single_sample_zero_variance_returns_min_buffer() {
        // CV = 0 â†’ buffer = 1.30 + 0 Ã— 3.50 = 1.30.
        let buf = l1_adaptive_buffer(&[50, 50, 50, 50]);
        assert!((buf - L1_BUFFER_MIN).abs() < 1e-9);
    }

    #[test]
    fn high_volatility_clamps_at_max_buffer() {
        // Wildly varying prices â†’ CV >> 1 â†’ buffer clamped at 2.00.
        let prices: Vec<u64> = (0..20).map(|i| if i % 2 == 0 { 1 } else { 1000 }).collect();
        let buf = l1_adaptive_buffer(&prices);
        assert!(buf >= 1.30 && buf <= L1_BUFFER_MAX,
            "buffer {} out of [1.30, 2.00]", buf);
        assert!((buf - L1_BUFFER_MAX).abs() < 0.01, "expected ~2.00, got {}", buf);
    }

    #[test]
    fn moderate_volatility_between_bounds() {
        let prices = vec![30u64, 35, 28, 40, 32, 38, 27, 42];
        let buf = l1_adaptive_buffer(&prices);
        assert!(buf >= L1_BUFFER_MIN && buf <= L1_BUFFER_MAX);
    }

    // â”€â”€ dynamic_min_profit â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn floor_respected_when_costs_are_below() {
        let profit = dynamic_min_profit(
            1_000_000, // base_min
            50_000,    // l2_exec_gas
            10_000,    // l1_data_gas
            1,         // current_l2_base_fee gwei
            1,         // current_l1_gas_price gwei
            1.30,      // l1_adaptive_buf
        );
        assert!(profit >= 1_000_000, "floor not respected: {}", profit);
    }

    #[test]
    fn high_l1_price_raises_min_profit() {
        let low  = dynamic_min_profit(0, 100_000, 50_000, 1, 10, 1.30);
        let high = dynamic_min_profit(0, 100_000, 50_000, 1, 200, 2.00);
        assert!(high > low, "high L1 price should raise min profit");
    }

    #[test]
    fn extraction_gas_included() {
        // With zero exec and data gas, only extraction gas should count.
        let profit = dynamic_min_profit(0, 0, 0, 100, 0, 1.30);
        let expected_ext = (EXTRACTION_GAS as f64 * 100.0 * L2_EXEC_BUFFER) as u64;
        assert_eq!(profit, expected_ext);
    }

    #[test]
    fn buffer_constants_match_spec() {
        assert_eq!(EXTRACTION_GAS, 45_000);
        assert!((L2_EXEC_BUFFER - 1.10).abs() < 1e-9);
        assert!((L1_BUFFER_MIN - 1.30).abs() < 1e-9);
        assert!((L1_BUFFER_MAX - 2.00).abs() < 1e-9);
    }

    // â”€â”€ fee_cap_variants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn conservative_is_0_7x() {
        let (cons, agg, emg) = fee_cap_variants(100);
        assert_eq!(cons, 70);
        assert_eq!(agg,  100);
        assert_eq!(emg,  200);
    }

    // â”€â”€ emergency_bundle_profitable â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn profitable_above_threshold() {
        // profit 1000, emergency_cost = 200 Ã— 5 = 1000, min = 50 â†’ 1000 > 1050 â†’ false
        assert!(!emergency_bundle_profitable(1000, 200, 5, 50));
    }

    #[test]
    fn profitable_well_above_threshold() {
        // profit 10_000, cost = 200 Ã— 5 = 1000, min = 50 â†’ 10_000 > 1050 â†’ true
        assert!(emergency_bundle_profitable(10_000, 200, 5, 50));
    }

    // â”€â”€ L1GasEma â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn ema_rolling_window_evicts_oldest() {
        let ema = L1GasEma::new(3);
        ema.push_price(10);
        ema.push_price(20);
        ema.push_price(30);
        ema.push_price(40); // evicts 10
        let snap = ema.history_snapshot();
        assert_eq!(snap, vec![20, 30, 40]);
    }

    #[test]
    fn ema_latest_gwei_returns_last() {
        let ema = L1GasEma::new(10);
        ema.push_price(55);
        ema.push_price(77);
        assert_eq!(ema.latest_gwei(), 77);
    }

    #[test]
    fn ema_empty_returns_zero() {
        let ema = L1GasEma::new(10);
        assert_eq!(ema.latest_gwei(), 0);
        assert!((ema.current_buffer() - L1_BUFFER_MIN).abs() < 1e-9);
    }
}