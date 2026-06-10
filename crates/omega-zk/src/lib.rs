// crates/omega-zk/src/lib.rs
// crates/omega-zk/src/lib.rs
//
// OmegaEngine v12.0 — omega-zk  (Layer 7 in the 14-layer stack)

pub mod config;
pub mod error;
pub mod prover;
pub mod verifier;
pub mod queue;
pub mod worker;
pub mod checkpoint;
pub mod commitment;
pub mod metrics;

pub use config::ZkConfig;
pub use error::ZkError;
pub use prover::{T1SoftwareProver, ZkProof, ProverTier};
pub use verifier::ZkVerifier;
pub use queue::{ProofQueue, QueuePressure, ProofRequest, ProofResponse};
pub use worker::ProofWorkerPool;
pub use checkpoint::ProofCheckpointManager;
pub use commitment::{compute_proof_commitment, ProofCommitment};