// crates/omega-security/src/metrics.rs
//
// Prometheus metrics for omega-security.
// All metrics are initialised lazily via once_cell::Lazy.
// Call register_all() once at process start to force eager initialisation
// and surface any registration conflicts early.

use once_cell::sync::Lazy;
use prometheus::{
    register_counter, register_counter_vec, register_gauge_vec,
    Counter, CounterVec, GaugeVec,
};

// â”€â”€â”€ Signing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total blueprints signed, labelled by the first 4 bytes of the signer address.
pub static BLUEPRINTS_SIGNED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_security_blueprints_signed_total",
        "Total blueprints signed by the execution key",
        &["signer_prefix"]
    )
    .expect("register omega_security_blueprints_signed_total")
});

/// Total signature verification failures.
pub static SIGNATURE_FAILURES: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_signature_failures_total",
        "Total secp256k1 signature verification failures"
    )
    .expect("register omega_security_signature_failures_total")
});

// â”€â”€â”€ Key management â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total key rotation events initiated.
pub static KEY_ROTATIONS: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_key_rotations_total",
        "Total execution-key rotation events (dual-key window openings)"
    )
    .expect("register omega_security_key_rotations_total")
});

/// Current key rotation state: 0 = Active, 1 = Rotating.
pub static KEY_ROTATION_STATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_security_key_rotation_state",
        "Current key rotation state (0=Active 1=Rotating)",
        &["chain_id"]
    )
    .expect("register omega_security_key_rotation_state")
});

// â”€â”€â”€ Replay protection â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total replay attempts detected (should always be zero in production).
pub static REPLAY_ATTEMPTS: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_replay_attempts_total",
        "Total blueprint replay attempts detected"
    )
    .expect("register omega_security_replay_attempts_total")
});

/// Total nonce mismatches detected.
pub static NONCE_MISMATCHES: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_nonce_mismatches_total",
        "Total blueprint nonce mismatch errors"
    )
    .expect("register omega_security_nonce_mismatches_total")
});

// â”€â”€â”€ OFA compliance â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// OFA compliance check outcomes, labelled by strategy and result.
pub static OFA_CHECKS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_security_ofa_checks_total",
        "OFA compliance check outcomes",
        &["strategy", "result"] // result: "pass" | "missing_consent" | "slippage" | "order" | "relay"
    )
    .expect("register omega_security_ofa_checks_total")
});

/// OFA rule set version currently loaded.
pub static OFA_RULE_VERSION: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_security_ofa_rule_version",
        "Currently loaded OFA rule set version",
        &["strategy"]
    )
    .expect("register omega_security_ofa_rule_version")
});

// â”€â”€â”€ Integrity â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total bytecode integrity check failures (Certora C4).
pub static BYTECODE_FAILURES: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_bytecode_failures_total",
        "Total bytecode hash mismatch failures (Certora C4)"
    )
    .expect("register omega_security_bytecode_failures_total")
});

/// Total strategy freeze events (Certora C7).
pub static STRATEGY_FREEZES: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_security_strategy_freezes_total",
        "Total strategy freeze events"
    )
    .expect("register omega_security_strategy_freezes_total")
});

/// Total attempts to use a frozen strategy.
pub static FROZEN_STRATEGY_ATTEMPTS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_security_frozen_strategy_attempts_total",
        "Attempts to submit a blueprint for a frozen strategy",
        &["strategy_id"]
    )
    .expect("register omega_security_frozen_strategy_attempts_total")
});

// â”€â”€â”€ Initialisation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Force eager initialisation of all metrics. Call once at process start.
pub fn register_all() {
    let _ = &*BLUEPRINTS_SIGNED;
    let _ = &*SIGNATURE_FAILURES;
    let _ = &*KEY_ROTATIONS;
    let _ = &*KEY_ROTATION_STATE;
    let _ = &*REPLAY_ATTEMPTS;
    let _ = &*NONCE_MISMATCHES;
    let _ = &*OFA_CHECKS;
    let _ = &*OFA_RULE_VERSION;
    let _ = &*BYTECODE_FAILURES;
    let _ = &*STRATEGY_FREEZES;
    let _ = &*FROZEN_STRATEGY_ATTEMPTS;
}