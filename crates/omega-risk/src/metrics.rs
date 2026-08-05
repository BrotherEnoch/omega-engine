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
    )
    .expect("register omega_risk_checks_passed_total")
});

/// Count of blueprints dropped at a pre-trade check, per strategy × drop_code.
pub static CHECKS_FAILED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_checks_failed_total",
        "Blueprints dropped at pre-trade check",
        &["strategy", "drop_code"]
    )
    .expect("register omega_risk_checks_failed_total")
});

// ─── Gas model ────────────────────────────────────────────────────────────────

/// Current L1 adaptive buffer per chain.
pub static L1_ADAPTIVE_BUFFER: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_l1_adaptive_buffer",
        "Current L1 adaptive gas buffer (1.30–2.00)",
        &["chain_id"]
    )
    .expect("register omega_risk_l1_adaptive_buffer")
});

/// Current dynamic minimum profit threshold per strategy.
pub static DYNAMIC_MIN_PROFIT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_dynamic_min_profit_gwei",
        "Current dynamic minimum profit threshold in gwei",
        &["strategy", "chain_id"]
    )
    .expect("register omega_risk_dynamic_min_profit_gwei")
});

// ─── Circuit breakers (EV-ratio based, per circuit_breakers.rs) ──────────────

/// Current circuit breaker state per strategy (0=Healthy 1=Investigate 2=AutoPaused 3=Halted).
pub static CIRCUIT_BREAKER_STATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_circuit_breaker_state",
        "Circuit breaker state (0=Healthy 1=Investigate 2=AutoPaused 3=Halted)",
        &["strategy"]
    )
    .expect("register omega_risk_circuit_breaker_state")
});

/// Rolling EV ratio per strategy.
pub static EV_RATIO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_ev_ratio",
        "Rolling EV ratio (observed/expected) over 72-block window",
        &["strategy"]
    )
    .expect("register omega_risk_ev_ratio")
});

/// Count of times a strategy's circuit breaker was resumed via L2
/// fast-approve (`resume_l2`). A Counter, not a Gauge — audit-trail
/// event, same reasoning as CIRCUIT_BREAKER_L3_CLEAR_TOTAL below.
/// Incremented directly from `StrategyCircuitBreaker::resume_l2`
/// (crates/omega-risk/src/circuit_breakers.rs).
pub static CIRCUIT_BREAKER_L2_RESUME_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_circuit_breaker_l2_resume_total",
        "Count of L2 fast-approve resumes (resume_l2) per strategy",
        &["strategy"]
    )
    .expect("register omega_risk_circuit_breaker_l2_resume_total")
});

/// Info-style metric carrying the operator and reason of the most recent
/// L2 resume for a strategy. Same "info metric" pattern as
/// CIRCUIT_BREAKER_L3_CLEAR_LAST_OPERATOR_INFO below.
pub static CIRCUIT_BREAKER_L2_RESUME_LAST_OPERATOR_INFO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_circuit_breaker_l2_resume_last_operator_info",
        "Info metric: value is always 1; operator/reason labels carry the most recent L2 resume's audit trail",
        &["strategy", "operator", "reason"]
    ).expect("register omega_risk_circuit_breaker_l2_resume_last_operator_info")
});

/// Count of times a strategy's circuit breaker was cleared via L3
/// governance (`clear_halt_l3`). A Counter, not a Gauge — this is an
/// audit-trail event, and a gauge that flips and resets can be missed
/// between Prometheus scrapes; a counter's `increase()` never loses the
/// event even if scraped well after the fact. Incremented directly from
/// `StrategyCircuitBreaker::clear_halt_l3` (crates/omega-risk/src/circuit_breakers.rs).
pub static CIRCUIT_BREAKER_L3_CLEAR_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_circuit_breaker_l3_clear_total",
        "Count of L3 governance clears (clear_halt_l3) per strategy",
        &["strategy"]
    )
    .expect("register omega_risk_circuit_breaker_l3_clear_total")
});

/// Info-style metric carrying the operator and reason of the most recent
/// L3 clear for a strategy, as label values rather than a numeric
/// measurement (the "info metric" pattern: value is always 1, the labels
/// ARE the payload) — same pattern as
/// KILL_SWITCH_RESET_LAST_OPERATOR_INFO below. One time series per
/// strategy, overwritten (not accumulated) on every clear, since only the
/// *latest* operator/reason is kept; CIRCUIT_BREAKER_L3_CLEAR_TOTAL
/// remains the reliable source for "how many times."
pub static CIRCUIT_BREAKER_L3_CLEAR_LAST_OPERATOR_INFO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_circuit_breaker_l3_clear_last_operator_info",
        "Info metric: value is always 1; operator/reason labels carry the most recent L3 clear's audit trail",
        &["strategy", "operator", "reason"]
    ).expect("register omega_risk_circuit_breaker_l3_clear_last_operator_info")
});

// ─── Kill switch (absolute funds-at-risk, per kill_switch.rs) ────────────────

/// 1 if the kill switch is currently tripped for a given scope, 0 otherwise.
/// Distinct from CIRCUIT_BREAKER_STATE: this reflects an absolute
/// funds-at-risk halt, not a relative EV-ratio degradation.
pub static KILL_SWITCH_TRIPPED: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_kill_switch_tripped",
        "1 if the kill switch is tripped for this scope, 0 otherwise",
        &["scope"]
    )
    .expect("register omega_risk_kill_switch_tripped")
});

/// Cumulative realized loss tracked by the kill switch, per scope, in wei.
pub static KILL_SWITCH_CUMULATIVE_LOSS_WEI: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_kill_switch_cumulative_loss_wei",
        "Cumulative realized loss tracked by the kill switch, in wei",
        &["scope"]
    )
    .expect("register omega_risk_kill_switch_cumulative_loss_wei")
});

/// Configured cumulative-loss trip threshold per scope, in wei. Published
/// once at first observation of a scope (see
/// KillSwitchRegistry::get_or_create) so alerting rules can compute
/// "observed / threshold" ratios (e.g. an early-warning at 80% of cap)
/// without hardcoding the threshold value into the alert rule itself.
pub static KILL_SWITCH_MAX_CUMULATIVE_LOSS_WEI: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_kill_switch_max_cumulative_loss_wei",
        "Configured cumulative-loss trip threshold for this scope, in wei",
        &["scope"]
    )
    .expect("register omega_risk_kill_switch_max_cumulative_loss_wei")
});

/// Count of times the kill switch was reset for a given scope. A Counter,
/// same reasoning as CIRCUIT_BREAKER_L3_CLEAR_TOTAL above: this is an
/// audit-trail event (someone decided it was safe to resume trading),
/// and it should be visible to the whole team even when the person who
/// pressed reset didn't separately announce it.
pub static KILL_SWITCH_RESET_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_risk_kill_switch_reset_total",
        "Count of kill switch resets per scope",
        &["scope"]
    )
    .expect("register omega_risk_kill_switch_reset_total")
});

/// Info-style metric carrying the operator and reason of the most recent
/// reset for a scope. Same "info metric" pattern as
/// CIRCUIT_BREAKER_L3_CLEAR_LAST_OPERATOR_INFO above.
pub static KILL_SWITCH_RESET_LAST_OPERATOR_INFO: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_kill_switch_reset_last_operator_info",
        "Info metric: value is always 1; operator/reason labels carry the most recent reset's audit trail",
        &["scope", "operator", "reason"]
    ).expect("register omega_risk_kill_switch_reset_last_operator_info")
});

// ─── Heartbeat / liveness (per heartbeat.rs) ─────────────────────────────────

/// Unix timestamp (seconds) of the last recorded beat for a given
/// component. Alert on `time() - this > <component's tolerance>` to
/// detect a crashed or hung process — distinct from every other metric in
/// this file, which describes activity outcomes and therefore reads as
/// "quiet" (not "dead") when nothing is happening.
pub static HEARTBEAT_LAST_BEAT_TIMESTAMP: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_heartbeat_last_beat_timestamp",
        "Unix timestamp (seconds) of the last liveness beat for this component",
        &["component"]
    )
    .expect("register omega_risk_heartbeat_last_beat_timestamp")
});

// ─── Flash crash ────────────────────────────────────────────────────────────

/// 1 when graduated flash-crash response is active for a given asset, 0 otherwise.
pub static FLASH_CRASH_ACTIVE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_risk_flash_crash_active",
        "1 when graduated flash-crash response is active",
        &["asset"]
    )
    .expect("register omega_risk_flash_crash_active")
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
    let _ = &*CIRCUIT_BREAKER_L2_RESUME_TOTAL;
    let _ = &*CIRCUIT_BREAKER_L2_RESUME_LAST_OPERATOR_INFO;
    let _ = &*CIRCUIT_BREAKER_L3_CLEAR_TOTAL;
    let _ = &*CIRCUIT_BREAKER_L3_CLEAR_LAST_OPERATOR_INFO;
    let _ = &*KILL_SWITCH_TRIPPED;
    let _ = &*KILL_SWITCH_CUMULATIVE_LOSS_WEI;
    let _ = &*KILL_SWITCH_MAX_CUMULATIVE_LOSS_WEI;
    let _ = &*KILL_SWITCH_RESET_TOTAL;
    let _ = &*KILL_SWITCH_RESET_LAST_OPERATOR_INFO;
    let _ = &*HEARTBEAT_LAST_BEAT_TIMESTAMP;
    let _ = &*FLASH_CRASH_ACTIVE;
}
