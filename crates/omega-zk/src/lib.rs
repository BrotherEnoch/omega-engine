// crates/omega-zk/src/lib.rs
//
// OmegaEngine v12.0 — omega-zk  (Layer 7 in the 14-layer stack)
//
// FIX (this revision, C4/C9 — see prover.rs/verifier.rs/binding.rs's own doc comments for
// the full reasoning): added the `binding` module, computing the off-chain mirror of
// OmegaVault.computePublicInputsHash(). Required by prover.rs's BlueprintPublicInputs (now
// commits public_inputs_hash_commitment alongside blueprint_hash) and by main.rs's new C9
// real ZK-gate enforcement.
//
// ## ZK integration (submit pathway)
//
//   `submit` encodes OmegaVault.submitProof calldata and buffers verified
//   proofs for an external signer/relayer. `ZkConfig.chain_id` is threaded into
//   ProofWorkerPool (no more hard-coded 42161). See docs/ZK_Proof_Integration.md.

pub mod binding;
pub mod checkpoint;
pub mod commitment;
pub mod config;
pub mod error;
pub mod metrics;
pub mod prover;
pub mod queue;
pub mod verifier;
pub mod worker;
pub mod submit;

pub use binding::{compute_public_inputs_hash, PUBLIC_INPUTS_VERSION};
pub use checkpoint::ProofCheckpointManager;
pub use commitment::{compute_proof_commitment, ProofCommitment};
pub use config::ZkConfig;
pub use error::ZkError;
pub use prover::{ProverTier, T1SoftwareProver, ZkProof};
pub use queue::{ProofQueue, ProofRequest, ProofResponse, QueuePressure};
pub use verifier::ZkVerifier;
pub use worker::ProofWorkerPool;
pub use submit::{
    encode_sp1_adapter_proof_blob, encode_sp1_public_values, encode_sp1_stark_proof_arg,
    encode_submit_proof_calldata, submit_proof_selector, PendingProofBuffer,
    VerifiedProofSubmission,
};