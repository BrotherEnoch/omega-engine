// crates/omega-zk/src/lib.rs
// crates/omega-zk/src/lib.rs
//
// OmegaEngine v12.0 — omega-zk  (Layer 7 in the 14-layer stack)

pub mod checkpoint;
pub mod commitment;
pub mod config;
pub mod error;
pub mod metrics;
pub mod prover;
pub mod queue;
pub mod verifier;
pub mod worker;

pub use checkpoint::ProofCheckpointManager;
pub use commitment::{compute_proof_commitment, ProofCommitment};
pub use config::ZkConfig;
pub use error::ZkError;
pub use prover::{ProverTier, T1SoftwareProver, ZkProof};
pub use queue::{ProofQueue, ProofRequest, ProofResponse, QueuePressure};
pub use verifier::ZkVerifier;
pub use worker::ProofWorkerPool;
