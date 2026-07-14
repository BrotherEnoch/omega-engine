// crates/omega-risk/src/gas_model.rs
// (unchanged except lines 258 and 267: manual_range_contains fixes;
//  and dynamic_min_profit: cost components now round UP, not truncate,
//  since this function computes a minimum-required-profit FLOOR — see
//  inline comment.)

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const EXTRACTION_GAS: u64 = 45_000;
pub const L2_EXEC_BUFFER: f64 = 1.10;
pub const L1_BUFFER_MIN:  f64 = 1.30;
pub const L1_BUFFER_MAX:  f64 = 2.00;
const L1_CV_SCALAR:       f64 = 3.50;
pub const MAX_PRIORITY_FEE_GWEI: u64 = 500;
pub const L1_EMA_WINDOW:  usize = 20;

pub fn dynamic_min_profit(
    base_min:             u64,
    l2_exec_gas:          u64,
    l1_data_gas:          u64,
    current_l2_base_fee:  u64,
    current_l1_gas_price: u64,
    l1_adaptive_buf:      f64,
) -> u64 {
    // Each component uses `.ceil()` rather than a bare `as u64` truncation.
    // This function computes a minimum-required-profit FLOOR: if the float
    // multiplication produces a fractional wei/gwei amount and we truncate
    // toward zero, the computed floor is slightly UNDER the true cost,
    // which means a marginally unprofitable trade could pass check 5
    // (MissProfit) by exactly the amount truncation discarded. Rounding
    // the cost estimate up is the conservative direction for a safety
    // floor — it can never let an unprofitable trade through due to
    // float-to-int rounding, only (rarely, by <1 wei-equivalent) reject
    // a trade that was truly break-even.
    let l2_cost  = (l2_exec_gas  as f64 * current_l2_base_fee  as f64 * L2_EXEC_BUFFER).ceil() as u64;
    let l1_cost  = (l1_data_gas  as f64 * current_l1_gas_price as f64 * l1_adaptive_buf).ceil() as u64;
    let ext_cost = (EXTRACTION_GAS as f64 * current_l2_base_fee as f64 * L2_EXEC_BUFFER).ceil() as u64;
    base_min.max(l2_cost + l1_cost + ext_cost)
}

pub fn l1_adaptive_buffer(l1_price_history: &[u64]) -> f64 {
    if l1_price_history.is_empty() { return L1_BUFFER_MIN; }

    let last = match l1_price_history.last() {
        Some(&v) if v > 0 => v as f64,
        _ => return L1_BUFFER_MIN,
    };

    let n    = l1_price_history.len() as f64;
    let mean = l1_price_history.iter().map(|&x| x as f64).sum::<f64>() / n;

    let variance = l1_price_history
        .iter()
        .map(|&x| { let d = x as f64 - mean; d * d })
        .sum::<f64>() / n;

    let std_dev = variance.sqrt();
    let cv      = std_dev / last.max(1.0);

    (L1_BUFFER_MIN + cv * L1_CV_SCALAR).clamp(L1_BUFFER_MIN, L1_BUFFER_MAX)
}

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

    pub fn push_price(&self, gwei: u64) {
        let mut g = self.inner.lock();
        if g.history.len() >= g.window_size { g.history.pop_front(); }
        g.history.push_back(gwei);
    }

    pub fn current_buffer(&self) -> f64 {
        let g     = self.inner.lock();
        let slice: Vec<u64> = g.history.iter().copied().collect();
        drop(g);
        l1_adaptive_buffer(&slice)
    }

    pub fn latest_gwei(&self) -> u64 {
        let g = self.inner.lock();
        g.history.back().copied().unwrap_or(0)
    }

    pub fn history_snapshot(&self) -> Vec<u64> {
        let g = self.inner.lock();
        g.history.iter().copied().collect()
    }

    pub fn len(&self) -> usize { self.inner.lock().history.len() }

    pub fn is_empty(&self) -> bool { self.inner.lock().history.is_empty() }
}

pub fn estimate_gas_cost_gwei(fee_cap_gwei: u64, gas_estimate: u64) -> u64 {
    fee_cap_gwei.saturating_mul(gas_estimate)
}

pub fn gwei_to_eth(gwei: u64) -> f64 {
    gwei as f64 / 1_000_000_000.0
}

pub fn fee_cap_variants(base_cap_gwei: u64) -> (u64, u64, u64) {
    let conservative = (base_cap_gwei as f64 * 0.7) as u64;
    let aggressive   = base_cap_gwei;
    let emergency    = base_cap_gwei.saturating_mul(2);
    (conservative, aggressive, emergency)
}

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

    #[test]
    fn empty_history_returns_min_buffer() {
        assert!((l1_adaptive_buffer(&[]) - L1_BUFFER_MIN).abs() < 1e-9);
    }

    #[test]
    fn single_sample_zero_variance_returns_min_buffer() {
        let buf = l1_adaptive_buffer(&[50, 50, 50, 50]);
        assert!((buf - L1_BUFFER_MIN).abs() < 1e-9);
    }

    #[test]
    fn high_volatility_clamps_at_max_buffer() {
        let prices: Vec<u64> = (0..20).map(|i| if i % 2 == 0 { 1 } else { 1000 }).collect();
        let buf = l1_adaptive_buffer(&prices);
        assert!((1.30..=L1_BUFFER_MAX).contains(&buf),
            "buffer {} out of [1.30, 2.00]", buf);
        assert!((buf - L1_BUFFER_MAX).abs() < 0.01, "expected ~2.00, got {}", buf);
    }

    #[test]
    fn moderate_volatility_between_bounds() {
        let prices = vec![30u64, 35, 28, 40, 32, 38, 27, 42];
        let buf = l1_adaptive_buffer(&prices);
        assert!((L1_BUFFER_MIN..=L1_BUFFER_MAX).contains(&buf));
    }

    #[test]
    fn floor_respected_when_costs_are_below() {
        let profit = dynamic_min_profit(1_000_000, 50_000, 10_000, 1, 1, 1.30);
        assert!(profit >= 1_000_000, "floor not respected: {}", profit);
    }

    #[test]
    fn high_l1_price_raises_min_profit() {
        let low  = dynamic_min_profit(0, 100_000, 50_000, 1, 10,  1.30);
        let high = dynamic_min_profit(0, 100_000, 50_000, 1, 200, 2.00);
        assert!(high > low, "high L1 price should raise min profit");
    }

    #[test]
    fn extraction_gas_included() {
        let profit       = dynamic_min_profit(0, 0, 0, 100, 0, 1.30);
        let expected_ext = (EXTRACTION_GAS as f64 * 100.0 * L2_EXEC_BUFFER).ceil() as u64;
        assert_eq!(profit, expected_ext);
    }

    #[test]
    fn rounding_never_undercounts_fractional_cost() {
        // l2_exec_gas=3, base_fee=1, buffer=1.10 -> raw = 3.3, truncation
        // would give 3; the floor must be at least 4 (ceil) so a
        // break-even-by-truncation trade can't slip through.
        let profit = dynamic_min_profit(0, 3, 0, 1, 0, 1.10);
        assert_eq!(profit, 4, "cost floor must round up, not truncate");
    }

    #[test]
    fn buffer_constants_match_spec() {
        assert_eq!(EXTRACTION_GAS, 45_000);
        assert!((L2_EXEC_BUFFER - 1.10).abs() < 1e-9);
        assert!((L1_BUFFER_MIN  - 1.30).abs() < 1e-9);
        assert!((L1_BUFFER_MAX  - 2.00).abs() < 1e-9);
    }

    #[test]
    fn conservative_is_0_7x() {
        let (cons, agg, emg) = fee_cap_variants(100);
        assert_eq!(cons, 70);
        assert_eq!(agg,  100);
        assert_eq!(emg,  200);
    }

    #[test]
    fn profitable_above_threshold() {
        assert!(!emergency_bundle_profitable(1000, 200, 5, 50));
    }

    #[test]
    fn profitable_well_above_threshold() {
        assert!(emergency_bundle_profitable(10_000, 200, 5, 50));
    }

    #[test]
    fn ema_rolling_window_evicts_oldest() {
        let ema = L1GasEma::new(3);
        ema.push_price(10);
        ema.push_price(20);
        ema.push_price(30);
        ema.push_price(40);
        assert_eq!(ema.history_snapshot(), vec![20, 30, 40]);
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