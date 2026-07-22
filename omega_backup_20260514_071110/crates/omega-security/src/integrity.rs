// crates/omega-security/src/integrity.rs
//
// Strategy integrity enforcement (spec S8 / OmegaOrchestrator.sol / Certora C4/C7/C8).
//
// Spec references:
//   OmegaOrchestrator.sol:
//     mapping(bytes32 => bytes32) public strategy_registry;          // stratId â†’ addr
//     mapping(bytes32 => bytes32) public strategy_bytecode_hashes;   // stratId â†’ hash
//     mapping(bytes32 => bool)    public strategy_frozen;            // freeze flag
//
//     execute():
//       require(!strategy_frozen[stratId], "STRATEGY_FROZEN");        // C7
//       require(keccak256(stratAddr.codehash) == bytecodeHash, "BYTECODE_MISMATCH"); // C4
//
//   Certora C4: "No delegatecall â€” strategy dispatch uses call only; bytecode integrity enforced"
//   Certora C7: "Frozen strategy reverts: blueprint with frozen stratId must always revert"
//   Certora C8: "Zero-capital invariant: orchestrator balance never decreases > gasCostUpperBound"
//              (structural invariant â€” enforced in Solidity; this module documents it)
//
// Design:
//   IntegrityRegistry stores (strategy_id â†’ expected_bytecode_hash) and the
//   per-strategy freeze flag.  Both are hot-reloadable (freeze is write-once via
//   governance; bytecode hashes require L3 48h timelock for changes).
//
//   StrategyFreezeGuard wraps IntegrityRegistry and provides the fast-path
//   freeze check used in the blueprint submission hot path (atomic read, no lock).
//
// Freeze semantics (spec S8):
//   Once frozen, a strategy CANNOT be unfrozen programmatically â€” this matches the
//   Solidity implementation where strategy_frozen is write-once (set to true; no
//   unfreeze function).  Governance can deploy a new strategy with a different ID.
//
// Thread-safety:
//   DashMap provides concurrent read/write.  Freeze is a write-once bool in a
//   DashSet so the "is_frozen" check is a lock-free set membership test.

use arc_swap::ArcSwap;
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::SecurityError;
use crate::metrics;
use crate::signer::keccak256;

// â”€â”€â”€ Strategy entry â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Registered strategy metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEntry {
    /// Strategy identifier string (e.g., "SA", "LA", "MEV").
    pub strategy_id: String,
    /// Expected keccak256 hash of the deployed strategy contract bytecode.
    /// Matches `strategy_bytecode_hashes[stratId]` in OmegaOrchestrator.sol.
    pub bytecode_hash: [u8; 32],
    /// Ethereum address of the deployed strategy contract.
    pub contract_address: [u8; 20],
    /// Phase at which this strategy becomes active (gates blueprint scoring).
    pub min_phase: u8,
}

// â”€â”€â”€ Integrity registry â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Strategy integrity registry.
///
/// `Arc<IntegrityRegistry>` is shared between the blueprint submission path,
/// the control-plane governance handler, and the health FSM.
pub struct IntegrityRegistry {
    /// strategy_id â†’ StrategyEntry (hot-swappable on L3 deployment).
    entries: DashMap<String, StrategyEntry>,
    /// Set of frozen strategy IDs (write-once; can never be removed).
    frozen:  DashSet<String>,
}

impl IntegrityRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: DashMap::new(),
            frozen:  DashSet::new(),
        })
    }

    // â”€â”€ Registration â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Register a strategy.
    ///
    /// Called at startup for each strategy in the active phase, and after L3
    /// governance approves a new strategy deployment.
    pub fn register(&self, entry: StrategyEntry) {
        let id = entry.strategy_id.clone();
        tracing::info!(
            strategy       = %id,
            phase          = entry.min_phase,
            contract       = hex::encode(entry.contract_address),
            bytecode_hash  = hex::encode(entry.bytecode_hash),
            "strategy registered in integrity registry"
        );
        self.entries.insert(id, entry);
    }

    /// Register multiple strategies at once (called from startup).
    pub fn register_all(&self, entries: impl IntoIterator<Item = StrategyEntry>) {
        for e in entries { self.register(e); }
    }

    // â”€â”€ Bytecode integrity check (Certora C4) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Verify that `actual_bytecode_hash` matches the registered expected hash
    /// for `strategy_id`.
    ///
    /// In production, `actual_bytecode_hash` is computed by the orchestrator
    /// as `keccak256(abi.encodePacked(stratAddr.codehash))` at execution time.
    /// The Rust simulation path calls this before submitting to relays.
    ///
    /// Returns:
    ///   `Ok(())`                     â€” hash matches; safe to execute.
    ///   `Err(BytecodeMismatch)`      â€” hash differs; HALT-worthy.
    ///   `Err(StrategyUnknown)`       â€” strategy not in registry.
    ///   `Err(StrategyFrozen)`        â€” strategy frozen; must be caught by freeze check first.
    pub fn check_bytecode(
        &self,
        strategy_id:         &str,
        actual_bytecode_hash: &[u8; 32],
    ) -> Result<(), SecurityError> {
        let entry = self.entries.get(strategy_id).ok_or_else(|| {
            SecurityError::StrategyUnknown { strategy_id: strategy_id.to_string() }
        })?;

        if &entry.bytecode_hash != actual_bytecode_hash {
            metrics::BYTECODE_FAILURES.inc();
            tracing::error!(
                strategy       = strategy_id,
                expected       = hex::encode(entry.bytecode_hash),
                actual         = hex::encode(actual_bytecode_hash),
                "BYTECODE INTEGRITY FAILURE (Certora C4)"
            );
            return Err(SecurityError::BytecodeMismatch {
                strategy_id: strategy_id.to_string(),
            });
        }
        Ok(())
    }

    /// Compute the expected blueprint-level bytecode hash from the strategy entry.
    /// This is `keccak256(strategy_contract_address || registered_bytecode_hash)`
    /// and is what the Orchestrator includes in `strategy_bytecode_hashes`.
    pub fn expected_hash_for_blueprint(&self, strategy_id: &str) -> Option<[u8; 32]> {
        self.entries.get(strategy_id).map(|e| {
            // Concatenate address + bytecode_hash and re-hash.
            let mut buf = Vec::with_capacity(20 + 32);
            buf.extend_from_slice(&e.contract_address);
            buf.extend_from_slice(&e.bytecode_hash);
            keccak256(&buf)
        })
    }

    // â”€â”€ Freeze management (Certora C7) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Freeze a strategy.
    ///
    /// Write-once: once frozen, cannot be unfrozen (spec C7 / Orchestrator.freezeStrategy).
    /// Requires DEFAULT_ADMIN_ROLE in the Orchestrator; here called by the L2
    /// governance handler after a signed freeze proposal.
    pub fn freeze(&self, strategy_id: &str) {
        self.frozen.insert(strategy_id.to_string());
        metrics::STRATEGY_FREEZES.inc();
        tracing::warn!(strategy = strategy_id, "strategy FROZEN â€” no further blueprints permitted");
    }

    /// True if the strategy is frozen.
    ///
    /// Hot path: DashSet membership test is O(1) and lock-free.
    #[inline]
    pub fn is_frozen(&self, strategy_id: &str) -> bool {
        self.frozen.contains(strategy_id)
    }

    /// Check freeze status â€” returns `Err(StrategyFrozen)` if frozen.
    ///
    /// Call this BEFORE `check_bytecode` in the blueprint pipeline
    /// (cheaper check first, matches on-chain require order in execute()).
    #[inline]
    pub fn check_frozen(&self, strategy_id: &str) -> Result<(), SecurityError> {
        if self.is_frozen(strategy_id) {
            metrics::FROZEN_STRATEGY_ATTEMPTS
                .with_label_values(&[strategy_id])
                .inc();
            return Err(SecurityError::StrategyFrozen {
                strategy_id: strategy_id.to_string(),
            });
        }
        Ok(())
    }

    /// Run both freeze + bytecode checks in the correct order.
    ///
    /// This is the primary entry point called by the blueprint submission path.
    pub fn full_integrity_check(
        &self,
        strategy_id:          &str,
        actual_bytecode_hash: &[u8; 32],
    ) -> Result<(), SecurityError> {
        self.check_frozen(strategy_id)?;     // C7 â€” cheap; first
        self.check_bytecode(strategy_id, actual_bytecode_hash)?; // C4 â€” lookup
        Ok(())
    }

    // â”€â”€ Observability â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Return all registered strategy IDs.
    pub fn registered_ids(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key().clone()).collect()
    }

    /// Return all frozen strategy IDs.
    pub fn frozen_ids(&self) -> Vec<String> {
        self.frozen.iter().map(|e| e.clone()).collect()
    }

    /// Return a snapshot of all registered entries (for the control-plane API).
    pub fn snapshot(&self) -> Vec<StrategyEntry> {
        self.entries.iter().map(|e| e.value().clone()).collect()
    }
}

impl Default for IntegrityRegistry {
    fn default() -> Self {
        Self {
            entries: DashMap::new(),
            frozen:  DashSet::new(),
        }
    }
}

// â”€â”€â”€ StrategyFreezeGuard â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Thin wrapper used in hot-path code that only needs the freeze check.
///
/// Clones the `Arc<IntegrityRegistry>` so the caller does not need to
/// import `IntegrityRegistry` directly.
#[derive(Clone)]
pub struct StrategyFreezeGuard {
    registry: Arc<IntegrityRegistry>,
}

impl StrategyFreezeGuard {
    pub fn new(registry: Arc<IntegrityRegistry>) -> Self {
        Self { registry }
    }

    /// True if `strategy_id` is frozen.  O(1) lock-free.
    #[inline]
    pub fn is_frozen(&self, strategy_id: &str) -> bool {
        self.registry.is_frozen(strategy_id)
    }

    /// Returns `Err(StrategyFrozen)` if frozen.
    #[inline]
    pub fn check(&self, strategy_id: &str) -> Result<(), SecurityError> {
        self.registry.check_frozen(strategy_id)
    }
}

// â”€â”€â”€ Default production strategy registrations â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Known bytecode hash seeds for the initial strategy registry.
/// In production these are read from the deployment artifacts and verified
/// against the deployed contract codehash via on-chain RPC before startup.
pub fn default_strategy_entries(phase: u8) -> Vec<StrategyEntry> {
    // Hash values are placeholder zeros â€” replaced with real deployed hashes
    // during the pre-audit deployment step (spec Section 21.3).
    let all = vec![
        StrategyEntry {
            strategy_id:      "CNRY".into(),
            bytecode_hash:    [0u8; 32], // populated from CanaryArb.sol deployment
            contract_address: [0u8; 20],
            min_phase:        0,
        },
        StrategyEntry {
            strategy_id:      "SA".into(),
            bytecode_hash:    [0u8; 32], // populated from SimpleArb.sol deployment
            contract_address: [0u8; 20],
            min_phase:        1,
        },
        StrategyEntry {
            strategy_id:      "MSA".into(),
            bytecode_hash:    [0u8; 32],
            contract_address: [0u8; 20],
            min_phase:        2,
        },
        StrategyEntry {
            strategy_id:      "LA".into(),
            bytecode_hash:    [0u8; 32],
            contract_address: [0u8; 20],
            min_phase:        3,
        },
        StrategyEntry {
            strategy_id:      "MEV".into(),
            bytecode_hash:    [0u8; 32],
            contract_address: [0u8; 20],
            min_phase:        4,
        },
    ];
    all.into_iter().filter(|e| e.min_phase <= phase).collect()
}

#[cfg(test)]
mod integrity_tests {
    use super::*;

    fn reg_with_sa() -> Arc<IntegrityRegistry> {
        let reg = IntegrityRegistry::new();
        reg.register(StrategyEntry {
            strategy_id:      "SA".into(),
            bytecode_hash:    [0xaa; 32],
            contract_address: [0x01; 20],
            min_phase:        1,
        });
        reg
    }

    // â”€â”€ Bytecode check (C4) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn correct_hash_passes_bytecode_check() {
        let reg = reg_with_sa();
        assert!(reg.check_bytecode("SA", &[0xaa; 32]).is_ok());
    }

    #[test]
    fn wrong_hash_fails_with_bytecode_mismatch() {
        let reg = reg_with_sa();
        assert!(matches!(
            reg.check_bytecode("SA", &[0xbb; 32]),
            Err(SecurityError::BytecodeMismatch { .. })
        ));
    }

    #[test]
    fn unknown_strategy_fails_with_strategy_unknown() {
        let reg = reg_with_sa();
        assert!(matches!(
            reg.check_bytecode("UNKNOWN", &[0xaa; 32]),
            Err(SecurityError::StrategyUnknown { .. })
        ));
    }

    // â”€â”€ Freeze (C7) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn unfrozen_strategy_passes_freeze_check() {
        let reg = reg_with_sa();
        assert!(reg.check_frozen("SA").is_ok());
    }

    #[test]
    fn frozen_strategy_fails_freeze_check() {
        let reg = reg_with_sa();
        reg.freeze("SA");
        assert!(matches!(
            reg.check_frozen("SA"),
            Err(SecurityError::StrategyFrozen { .. })
        ));
    }

    #[test]
    fn freeze_is_permanent() {
        let reg = reg_with_sa();
        reg.freeze("SA");
        // Freeze again (idempotent) â€” still frozen
        reg.freeze("SA");
        assert!(reg.is_frozen("SA"));
    }

    #[test]
    fn freezing_one_strategy_does_not_affect_others() {
        let reg = IntegrityRegistry::new();
        for id in ["SA", "LA", "MEV"] {
            reg.register(StrategyEntry {
                strategy_id:      id.into(),
                bytecode_hash:    [0xcc; 32],
                contract_address: [0x02; 20],
                min_phase:        1,
            });
        }
        reg.freeze("MEV");
        assert!(reg.is_frozen("MEV"));
        assert!(!reg.is_frozen("SA"));
        assert!(!reg.is_frozen("LA"));
    }

    // â”€â”€ Full integrity check â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn frozen_fails_before_bytecode_check() {
        let reg = reg_with_sa();
        reg.freeze("SA");
        // Even with the correct bytecode hash, freeze check fails first.
        assert!(matches!(
            reg.full_integrity_check("SA", &[0xaa; 32]),
            Err(SecurityError::StrategyFrozen { .. })
        ));
    }

    #[test]
    fn unfrozen_correct_hash_full_check_passes() {
        let reg = reg_with_sa();
        assert!(reg.full_integrity_check("SA", &[0xaa; 32]).is_ok());
    }

    #[test]
    fn unfrozen_wrong_hash_full_check_fails_with_mismatch() {
        let reg = reg_with_sa();
        assert!(matches!(
            reg.full_integrity_check("SA", &[0xdd; 32]),
            Err(SecurityError::BytecodeMismatch { .. })
        ));
    }

    // â”€â”€ StrategyFreezeGuard â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn freeze_guard_reflects_registry_state() {
        let reg   = reg_with_sa();
        let guard = StrategyFreezeGuard::new(Arc::clone(&reg));
        assert!(!guard.is_frozen("SA"));
        reg.freeze("SA");
        assert!(guard.is_frozen("SA"));
    }

    // â”€â”€ Observability â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[test]
    fn registered_ids_returns_all() {
        let reg = reg_with_sa();
        reg.register(StrategyEntry {
            strategy_id:      "LA".into(),
            bytecode_hash:    [0xbb; 32],
            contract_address: [0x02; 20],
            min_phase:        3,
        });
        let ids = reg.registered_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"SA".to_string()));
        assert!(ids.contains(&"LA".to_string()));
    }

    #[test]
    fn frozen_ids_lists_only_frozen() {
        let reg = reg_with_sa();
        reg.register(StrategyEntry {
            strategy_id:      "LA".into(),
            bytecode_hash:    [0xbb; 32],
            contract_address: [0x02; 20],
            min_phase:        3,
        });
        reg.freeze("LA");
        let frozen = reg.frozen_ids();
        assert_eq!(frozen, vec!["LA".to_string()]);
    }

    #[test]
    fn expected_hash_for_blueprint_is_deterministic() {
        let reg = reg_with_sa();
        let h1 = reg.expected_hash_for_blueprint("SA");
        let h2 = reg.expected_hash_for_blueprint("SA");
        assert_eq!(h1, h2);
        assert!(h1.is_some());
    }

    #[test]
    fn default_phase0_entries_includes_cnry_only() {
        let entries = default_strategy_entries(0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].strategy_id, "CNRY");
    }

    #[test]
    fn default_phase4_entries_includes_all_strategies() {
        let entries = default_strategy_entries(4);
        assert_eq!(entries.len(), 5);
    }
}