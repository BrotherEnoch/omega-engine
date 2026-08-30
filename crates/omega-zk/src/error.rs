// crates/omega-zk/src/error.rs

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZkError {
    // ── Prover ────────────────────────────────────────────────────────────────
    #[error("Proof generation failed for blueprint {blueprint_hash}: {detail}")]
    ProofGenerationFailed {
        blueprint_hash: String,
        detail: String,
    },

    #[error("Proof generation timed out after {elapsed_ms}ms (SLA: {sla_ms}ms)")]
    ProofTimeout { elapsed_ms: u64, sla_ms: u64 },

    #[error("Unsupported prover tier: {tier}")]
    UnsupportedProverTier { tier: String },

    // ── Verifier ──────────────────────────────────────────────────────────────
    #[error("Proof verification failed for blueprint {blueprint_hash}")]
    VerificationFailed { blueprint_hash: String },

    #[error("Proof length invalid: expected ≥{min_bytes} bytes, got {got_bytes}")]
    InvalidProofLength { min_bytes: usize, got_bytes: usize },

    // ── Queue ──────────────────────────────────────────────────────────────────
    #[error("Proof queue is full (depth {depth}, halt threshold {halt_threshold})")]
    QueueFull { depth: usize, halt_threshold: usize },

    #[error("Proof queue suspended — non-hot-path proofs rejected (depth {depth})")]
    QueueSuspended { depth: usize },

    #[error("Proof request {request_id} cancelled — blueprint expired")]
    RequestCancelled { request_id: u64 },

    #[error("Proof request {request_id} not found in queue")]
    RequestNotFound { request_id: u64 },

    // ── Worker pool ───────────────────────────────────────────────────────────
    #[error("Worker pool shut down — no workers available")]
    PoolShutdown,

    #[error("Worker {worker_id} panicked on request {request_id}")]
    WorkerPanic { worker_id: usize, request_id: u64 },

    // ── Checkpoint ───────────────────────────────────────────────────────────
    #[error("Checkpoint write failed for request {request_id}: {detail}")]
    CheckpointWriteFailed { request_id: u64, detail: String },

    #[error("Checkpoint read failed: {detail}")]
    CheckpointReadFailed { detail: String },

    // ── Commitment ────────────────────────────────────────────────────────────
    #[error("Commitment derivation failed for blueprint {blueprint_hash}: {detail}")]
    CommitmentFailed {
        blueprint_hash: String,
        detail: String,
    },

    // ── On-chain submission buffer ────────────────────────────────────────────
    #[error("Pending proof submission buffer full (capacity {capacity})")]
    SubmissionBufferFull { capacity: usize },

    // ── Internal ──────────────────────────────────────────────────────────────
    #[error("Internal ZK error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl ZkError {
    /// True for errors that propagate HALT upward to L0 Health FSM.
    pub fn is_halt_worthy(&self) -> bool {
        matches!(self, ZkError::QueueFull { .. } | ZkError::PoolShutdown)
    }

    /// True for errors that move the ZK layer to DEGRADED (not full HALT).
    pub fn is_degraded_worthy(&self) -> bool {
        matches!(
            self,
            ZkError::QueueSuspended { .. } | ZkError::ProofTimeout { .. }
        )
    }
}