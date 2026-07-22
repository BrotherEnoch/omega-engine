// crates/omega-zk/src/metrics.rs

use once_cell::sync::Lazy;
use prometheus::{
    register_counter, register_counter_vec, register_gauge, register_gauge_vec,
    register_histogram_vec, Counter, CounterVec, Gauge, GaugeVec, HistogramVec,
};

// â”€â”€â”€ Queue pressure â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Current proof queue depth.
pub static QUEUE_DEPTH: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!(
        "omega_zk_queue_depth",
        "Current proof request queue depth"
    ).expect("register omega_zk_queue_depth")
});

/// Current queue pressure state: 0=Normal 1=Throttle 2=Suspend 3=Halt.
pub static QUEUE_PRESSURE_STATE: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!(
        "omega_zk_queue_pressure_state",
        "Queue pressure FSM state (0=Normal 1=Throttle 2=Suspend 3=Halt)"
    ).expect("register omega_zk_queue_pressure_state")
});

/// Total requests enqueued.
pub static REQUESTS_ENQUEUED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_requests_enqueued_total",
        "Total proof requests enqueued",
        &["lane", "strategy"]
    ).expect("register omega_zk_requests_enqueued_total")
});

/// Total requests rejected (queue full or suspended).
pub static REQUESTS_REJECTED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_requests_rejected_total",
        "Proof requests rejected due to queue pressure",
        &["reason"] // "queue_full" | "suspended"
    ).expect("register omega_zk_requests_rejected_total")
});

/// Total requests skipped (shadow mode or hot-path skip allowed).
pub static REQUESTS_SKIPPED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_requests_skipped_total",
        "Proof requests skipped (shadow mode or explicit skip)",
        &["strategy"]
    ).expect("register omega_zk_requests_skipped_total")
});

// â”€â”€â”€ Proof generation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total proofs generated successfully.
pub static PROOFS_GENERATED: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_proofs_generated_total",
        "Total ZK proofs generated successfully",
        &["lane", "strategy", "prover_tier"]
    ).expect("register omega_zk_proofs_generated_total")
});

/// Total proof generation failures.
pub static PROOF_FAILURES: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_proof_failures_total",
        "Total ZK proof generation failures",
        &["reason"] // "timeout" | "prover_error" | "cancelled"
    ).expect("register omega_zk_proof_failures_total")
});

/// Proof generation latency in milliseconds.
pub static PROOF_LATENCY_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "omega_zk_proof_latency_ms",
        "ZK proof generation latency in milliseconds",
        &["lane", "strategy"],
        vec![50.0, 100.0, 200.0, 500.0, 1000.0, 1200.0, 2000.0, 4000.0, 8000.0]
    ).expect("register omega_zk_proof_latency_ms")
});

/// Proof SLA violations (latency > SLA target).
pub static PROOF_SLA_VIOLATIONS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "omega_zk_proof_sla_violations_total",
        "Proof generation exceeded SLA target",
        &["lane"]
    ).expect("register omega_zk_proof_sla_violations_total")
});

// â”€â”€â”€ Verification â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total proofs verified successfully.
pub static PROOFS_VERIFIED: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_zk_proofs_verified_total",
        "Total ZK proofs verified successfully"
    ).expect("register omega_zk_proofs_verified_total")
});

/// Total proof verification failures.
pub static VERIFICATION_FAILURES: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_zk_verification_failures_total",
        "Total ZK proof verification failures"
    ).expect("register omega_zk_verification_failures_total")
});

// â”€â”€â”€ Worker pool â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Number of currently active worker tasks.
pub static WORKER_COUNT: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "omega_zk_worker_count",
        "Number of active proof worker tasks",
        &["state"] // "idle" | "proving"
    ).expect("register omega_zk_worker_count")
});

/// Total worker panics (should always be zero).
pub static WORKER_PANICS: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_zk_worker_panics_total",
        "Total worker task panics (Certora: should be zero)"
    ).expect("register omega_zk_worker_panics_total")
});

// â”€â”€â”€ Checkpoints â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Total checkpoints written.
pub static CHECKPOINTS_WRITTEN: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_zk_checkpoints_written_total",
        "Total proof checkpoints written to disk"
    ).expect("register omega_zk_checkpoints_written_total")
});

/// Total checkpoints recovered on startup.
pub static CHECKPOINTS_RECOVERED: Lazy<Counter> = Lazy::new(|| {
    register_counter!(
        "omega_zk_checkpoints_recovered_total",
        "Total proof checkpoints recovered from disk on startup"
    ).expect("register omega_zk_checkpoints_recovered_total")
});

// â”€â”€â”€ Initialisation â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub fn register_all() {
    let _ = &*QUEUE_DEPTH;
    let _ = &*QUEUE_PRESSURE_STATE;
    let _ = &*REQUESTS_ENQUEUED;
    let _ = &*REQUESTS_REJECTED;
    let _ = &*REQUESTS_SKIPPED;
    let _ = &*PROOFS_GENERATED;
    let _ = &*PROOF_FAILURES;
    let _ = &*PROOF_LATENCY_MS;
    let _ = &*PROOF_SLA_VIOLATIONS;
    let _ = &*PROOFS_VERIFIED;
    let _ = &*VERIFICATION_FAILURES;
    let _ = &*WORKER_COUNT;
    let _ = &*WORKER_PANICS;
    let _ = &*CHECKPOINTS_WRITTEN;
    let _ = &*CHECKPOINTS_RECOVERED;
}