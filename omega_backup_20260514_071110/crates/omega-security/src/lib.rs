ï»¿// crates/omega-security/src/lib.rs
//
// OmegaEngine v12.0 â€” omega-security  (Layer 3 in the 14-layer stack)
//
// Responsibility: blueprint authentication, replay prevention, execution-key
// management, OFA compliance enforcement, and versioned-rule governance.
//
// Spec coverage:
//   Section 2   â€” Layer 3: Security (sits between RPC and Compliance in the stack).
//   Section 3   â€” Health FSM: chain-scoped nonces; execution-key dual-rotation window.
//   Section 5   â€” Governance: L2/L3 key-rotation path; emergency L3 path.
//   Section 8   â€” Security + OFA compliance + versioned OFA rule set.
//   Section 12  â€” Gas War Engine: bundle signing before relay submission.
//   Orchestrator.sol â€” EXECUTOR_ROLE, replay DashMap, dual-key window, bytecode hash guard.
//   Certora C4  â€” No delegatecall; bytecode integrity enforced at call site.
//   Certora C5  â€” Replay impossibility: executed_blueprints[hash] is set once.
//   Certora C7  â€” Frozen strategy reverts.
//   Certora C8  â€” Zero-capital invariant (orchestrator never holds ETH).
//
// Module layout:
//   signer         â€” secp256k1 blueprint signing + Flashbots header generation.
//   key_manager    â€” execution-key + dual-key rotation window (spec S3 / S5).
//   replay         â€” chain-scoped nonce registry + replay DashSet (spec Certora C5).
//   ofa            â€” versioned OFA rule registry + compliance check (spec S8).
//   integrity      â€” bytecode-hash guard + strategy freeze registry (spec Certora C4/C7).
//   metrics        â€” Prometheus counters for every security event.
//   error          â€” unified SecurityError type.

pub mod error;
pub mod signer;
pub mod key_manager;
pub mod replay;
pub mod ofa;
pub mod integrity;
pub mod metrics;

pub use error::SecurityError;
pub use signer::{BlueprintSigner, SignedBundle};
pub use key_manager::{KeyManager, KeyRotationState};
pub use replay::{ReplayGuard, NonceRegistry};
pub use ofa::{OfaRuleRegistry, OfaComplianceResult, OfaRuleSet};
pub use integrity::{IntegrityRegistry, StrategyFreezeGuard};

#[cfg(test)]
mod tests;