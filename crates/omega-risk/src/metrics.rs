// crates/omega-risk/src/metrics.rs
//
// Prometheus metrics for the risk layer.
// All metrics are labelled with strategy_id and, where applicable, drop_code.
// Registered once at process start via register_all().

use once_cell::sync::Lazy;
use prometheus::{register_counter_vec, register_gauge_vec, CounterVec, GaugeVec};

// ─── Pre-trade check outcomes ─────────────────────────────────────────────────

/// Count of blueprints that passed all 13 checks, per strategy.
pub static CHECKS_PASSED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_checks_passed_total",
        "Blueprints that passed all 13 pre-trade checks",
        &["strategy"]
    ).expect("register omega_risk_checks_passed_total")
});

/// Count of blueprints dropped at a pre-trade check, per strategy × drop_code.
pub static CHECKS_FAILED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_checks_failed_total",
        "Blueprints dropped at pre-trade check",
        &["strategy", "drop_code"]
    ).expect("register omega_risk_checks_failed_total")
});

// ─── Gas model ────────────────────────────────────────────────────────────────

/// Current L1 adaptive buffer per chain.
pub static L1_ADAPTIVE_BUFFER: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_l1_adaptive_buffer",
        "Current L1 adaptive gas buffer (1.30–2.00)",
        &["chain_id"]
    ).expect("register omega_risk_l1_adaptive_buffer")
});

/// Current dynamic minimum profit threshold per strategy.
pub static DYNAMIC_MIN_PROFIT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_dynamic_min_profit_gwei",
        "Current dynamic minimum profit threshold in gwei",
        &["strategy", "chain_id"]
    ).expect("register omega_risk_dynamic_min_profit_gwei")
});

// ─── Circuit breakers ────────────────────────────────────────────────────────

/// Current circuit breaker state per strategy (0=Healthy 1=Investigate 2=AutoPaused 3=Halted).
pub static CIRCUIT_BREAKER_STATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_circuit_breaker_state",
        "Circuit breaker state (0=Healthy 1=Investigate 2=AutoPaused 3=Halted)",
        &["strategy"]
    ).expect("register omega_risk_circuit_breaker_state")
});

/// Rolling EV ratio per strategy.
pub static EV_RATIO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_ev_ratio",
        "Rolling EV ratio (observed/expected) over 72-block window",
        &["strategy"]
    ).expect("register omega_risk_ev_ratio")
});

// ─── Flash crash ────────────────────────────────────────────────────────────

/// 1 when graduated flash-crash response is active for a given asset, 0 otherwise.
pub static FLASH_CRASH_ACTIVE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_flash_crash_active",
        "1 when graduated flash-crash response is active",
        &["asset"]
    ).expect("register omega_risk_flash_crash_active")
});

// ─── Initialisation ───────────────────────────────────────────────────────────

/// Force lazy initialisation of all metrics. Call once at process start.
pub fn register_all() {
    let _ = &*CHECKS_PASSED;
    let _ = &*CHECKS_FAILED;
    let _ = &*L1_ADAPTIVE_BUFFER;
    let _ = &*DYNAMIC_MIN_PROFIT;
    let _ = &*CIRCUIT_BREAKER_STATE;
    let _ = &*EV_RATIO;
    let _ = &*FLASH_CRASH_ACTIVE;
}