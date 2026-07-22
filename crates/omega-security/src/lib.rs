// crates/omega-security/src/lib.rs
//
// OmegaEngine v12.0 — omega-security (Layer 3 in the 14-layer stack)
//
// Responsibilities:
//   §8  — OFA compliance + versioned rule registry
//   §8  — Strategy bytecode integrity (Certora C4/C7)
//   §3  — Execution-key management + dual-key rotation window
//   §3  — Blueprint signing (EIP-191 / secp256k1)
//   §3  — Replay guard (Certora C5)
//   §3  — Nonce registry (chain-scoped per-strategy)
//   §16 — Prometheus metrics
//
// ## Audit fix (this revision)
//
// Re-exports `StrategyDeployment` / `DeploymentManifest` /
// `strategy_entries_from_manifest` alongside the existing integrity exports —
// see `integrity.rs`'s own audit note for why the old placeholder-shipping
// `default_strategy_entries()` was removed and replaced with these.

pub mod error;
pub mod integrity;
pub mod key_manager;
pub mod metrics;
pub mod ofa;
pub mod replay;
pub mod signer;

#[cfg(test)]
mod tests;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use error::SecurityError;
pub use integrity::{
    strategy_entries_from_manifest, DeploymentManifest, IntegrityRegistry, StrategyDeployment,
    StrategyEntry, StrategyFreezeGuard,
};
pub use key_manager::{KeyManager, KeyRotationState, ROTATION_WINDOW_BLOCKS};
pub use ofa::{
    default_rule_set, OfaComplianceInput, OfaComplianceResult, OfaRule, OfaRuleRegistry,
    OfaRuleSet,
};
pub use replay::{NonceRegistry, NonceState, ReplayGuard};
pub use signer::{
    blueprint_hash, eip191_hash, keccak256, pubkey_to_address, secret_key_to_address,
    BlueprintSigner, Signature, SignedBundle,
};