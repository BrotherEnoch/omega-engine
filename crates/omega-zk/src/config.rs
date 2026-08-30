// crates/omega-zk/src/config.rs
//
// ZK layer configuration (spec config/default.toml [zk] section).
//
// All constant values match the spec exactly:
//   prover_tier          = "t1_software"
//   microtx_sla_ms       = 1200
//   normal_sla_ms        = 4000
//   proof_queue_throttle = 128
//   proof_queue_suspend  = 256
//   proof_queue_halt     = 512

use serde::{Deserialize, Serialize};

// ─── Spec constants (never change without a governance vote) ──────────────────

/// Microtx lane proof SLA in milliseconds (spec: 1200 ms).
pub const MICROTX_SLA_MS: u64 = 1200;

/// Normal lane proof SLA in milliseconds (spec: 4000 ms).
pub const NORMAL_SLA_MS: u64 = 4000;

/// Queue depth at which throttling begins (spec: 128).
pub const QUEUE_THROTTLE_DEPTH: usize = 128;

/// Queue depth at which the prover suspends non-hot-path requests (spec: 256).
pub const QUEUE_SUSPEND_DEPTH: usize = 256;

/// Queue depth at which the proof pipeline halts and emits L0 HALT (spec: 512).
pub const QUEUE_HALT_DEPTH: usize = 512;

// ─── Prover tier ──────────────────────────────────────────────────────────────

/// Prover tier identifier (spec: "t1_software" baseline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProverTierConfig {
    /// In-process Winterfell STARK (spec baseline, Phase 1–2).
    #[default]
    T1Software,
    /// GPU/FPGA offload (Phase 3+; same ZkProof interface).
    T1Hardware,
}

impl ProverTierConfig {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProverTierConfig::T1Software => "t1_software",
            ProverTierConfig::T1Hardware => "t1_hardware",
        }
    }
}

// ─── Full ZK configuration ────────────────────────────────────────────────────

/// Full ZK layer configuration loaded from config/default.toml [zk].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZkConfig {
    /// Active prover tier (spec default: T1Software).
    pub prover_tier: ProverTierConfig,

    /// Microtx lane proof SLA in milliseconds (spec: 1200).
    pub microtx_sla_ms: u64,

    /// Normal lane proof SLA in milliseconds (spec: 4000).
    pub normal_sla_ms: u64,

    /// Queue depth at which new requests are throttled (spec: 128).
    pub proof_queue_throttle: usize,

    /// Queue depth at which non-hot-path proofs are suspended (spec: 256).
    pub proof_queue_suspend: usize,

    /// Queue depth at which the pipeline halts entirely (spec: 512).
    pub proof_queue_halt: usize,

    /// Number of async worker tasks in the proof worker pool.
    /// Default: number of available CPU cores, capped at 8 for T1Software.
    pub worker_count: usize,

    /// Directory for proof checkpoint files (in-progress proof state).
    pub checkpoint_dir: String,

    /// Maximum number of proof checkpoints to retain on disk.
    pub max_checkpoints: usize,

    /// Whether to skip ZK proof generation for Microtx blueprints during
    /// Phase 0 shadow mode (never skip in Phase 1+).
    pub allow_skip_in_shadow: bool,

    /// Chain ID the proof AIR and verifier bind to. MUST match the process-wide
    /// chain (e.g. 42161 Arbitrum One). Previously hardcoded inside
    /// `ProofWorkerPool::start`; now explicit so a mis-set OMEGA_CHAIN_ID cannot
    /// silently produce proofs for the wrong chain.
    pub chain_id: u64,
}

impl Default for ZkConfig {
    fn default() -> Self {
        Self {
            prover_tier: ProverTierConfig::T1Software,
            microtx_sla_ms: MICROTX_SLA_MS,
            normal_sla_ms: NORMAL_SLA_MS,
            proof_queue_throttle: QUEUE_THROTTLE_DEPTH,
            proof_queue_suspend: QUEUE_SUSPEND_DEPTH,
            proof_queue_halt: QUEUE_HALT_DEPTH,
            worker_count: num_cpus(),
            checkpoint_dir: "/var/omega/zk-checkpoints".into(),
            max_checkpoints: 64,
            allow_skip_in_shadow: true,
            chain_id: 42_161,
        }
    }
}

impl ZkConfig {
    /// Return the SLA for the given lane (microtx vs normal).
    pub fn sla_ms(&self, is_microtx: bool) -> u64 {
        if is_microtx {
            self.microtx_sla_ms
        } else {
            self.normal_sla_ms
        }
    }
}

/// Detect CPU count capped at 8 for the T1Software prover.
fn num_cpus() -> usize {
    // Capped at 8: more threads do not help the in-process STARK prover
    // (it is CPU-bound per proof; parallelism comes from multiple proofs).
    std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(4)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_matches_spec_exactly() {
        let cfg = ZkConfig::default();
        assert_eq!(cfg.microtx_sla_ms, MICROTX_SLA_MS);
        assert_eq!(cfg.normal_sla_ms, NORMAL_SLA_MS);
        assert_eq!(cfg.proof_queue_throttle, QUEUE_THROTTLE_DEPTH);
        assert_eq!(cfg.proof_queue_suspend, QUEUE_SUSPEND_DEPTH);
        assert_eq!(cfg.proof_queue_halt, QUEUE_HALT_DEPTH);
        assert_eq!(cfg.prover_tier, ProverTierConfig::T1Software);
        assert_eq!(cfg.chain_id, 42_161);
    }

    #[test]
    fn spec_constants_correct() {
        assert_eq!(MICROTX_SLA_MS, 1200);
        assert_eq!(NORMAL_SLA_MS, 4000);
        assert_eq!(QUEUE_THROTTLE_DEPTH, 128);
        assert_eq!(QUEUE_SUSPEND_DEPTH, 256);
        assert_eq!(QUEUE_HALT_DEPTH, 512);
    }

    #[test]
    fn sla_ms_dispatches_correctly() {
        let cfg = ZkConfig::default();
        assert_eq!(cfg.sla_ms(true), 1200);
        assert_eq!(cfg.sla_ms(false), 4000);
    }

    #[test]
    fn worker_count_in_valid_range() {
        let cfg = ZkConfig::default();
        assert!(cfg.worker_count >= 1 && cfg.worker_count <= 8);
    }
}