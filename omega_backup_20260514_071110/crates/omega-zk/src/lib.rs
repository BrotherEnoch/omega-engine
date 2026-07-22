ï»¿// crates/omega-zk/src/lib.rs
//
// OmegaEngine v12.0 â€” omega-zk  (Layer 7 in the 14-layer stack)
//
// Responsibility: ZK proof generation, queue management, SLA enforcement,
// and proof verification for the Vault's starkVerifier gate.
//
// Spec coverage:
//   Section 1.1  â€” "T1 ZK" key differentiator for Phase 1.
//   Section 2    â€” Layer 7: ZK sits between DAG (L6) and HotPath (L8).
//   Section 4    â€” Performance: zero-copy, lock-free queues, CPU-pinned workers.
//   config [zk]  â€” prover_tier = "t1_software"; SLA targets; queue thresholds.
//   OmegaVault   â€” starkVerifier.verify(starkProof, blueprintHash, netProfit).
//   ExecutionBlueprint â€” zk_proof_commitment: Option<B256>.
//   Section 7    â€” "ZK proof async worker pool (T1 software baseline)".
//                  "proof queue auto-throttle + checkpoint manager".
//
// Queue pressure levels (spec config/default.toml):
//   depth < 128  â†’ Normal     â€” all blueprints proceed.
//   128 â‰¤ depth < 256 â†’ Throttle  â€” new blueprints slow-submitted; skip allowed.
//   256 â‰¤ depth < 512 â†’ Suspend   â€” non-hot-path proofs suspended; emit DEGRADED.
//   depth â‰¥ 512  â†’ Halt       â€” entire proof pipeline paused; L0 HALT propagated.
//
// Prover tiers (extensible for hardware acceleration in Phase 3+):
//   T1Software   â€” Winterfell STARK in-process (current).
//   T1Hardware   â€” GPU/FPGA offload (Phase 3+ placeholder: same interface).
//
// SLA targets (spec):
//   Microtx lane â€” 1200 ms.
//   Normal lane  â€” 4000 ms.
//
// Module layout:
//   config        â€” ZkConfig loaded from config/default.toml [zk] section.
//   prover        â€” T1 software STARK prover (Winterfell backend).
//   verifier      â€” STARK proof verifier (used by Vault simulation path).
//   queue         â€” Lock-free proof request queue with pressure FSM.
//   worker        â€” Async worker pool: N tokio tasks, each consuming from the queue.
//   checkpoint    â€” Proof checkpoint manager: persist/recover in-progress proofs.
//   commitment    â€” Blueprint â†’ proof commitment derivation.
//   metrics       â€” Prometheus gauges/counters/histograms for every ZK event.
//   error         â€” ZkError unified error type.

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

#[cfg(test)]
#[cfg(test)]
mod tests;