// crates/omega-security/src/integrity.rs
//
// Strategy integrity enforcement (spec S8 / OmegaOrchestrator.sol / Certora C4/C7/C8).
//
// Spec references:
//   OmegaOrchestrator.sol:
//     mapping(bytes32 => bytes32) public strategy_registry;          // stratId → addr
//     mapping(bytes32 => bytes32) public strategy_bytecode_hashes;   // stratId → hash
//     mapping(bytes32 => bool)    public strategy_frozen;            // freeze flag
//
//     execute():
//       require(!strategy_frozen[stratId], "STRATEGY_FROZEN");        // C7
//       require(keccak256(stratAddr.codehash) == bytecodeHash, "BYTECODE_MISMATCH"); // C4
//
//   Certora C4: "No delegatecall — strategy dispatch uses call only; bytecode integrity enforced"
//   Certora C7: "Frozen strategy reverts: blueprint with frozen stratId must always revert"
//   Certora C8: "Zero-capital invariant: orchestrator balance never decreases > gasCostUpperBound"
//              (structural invariant — enforced in Solidity; this module documents it)
//
// Design:
//   IntegrityRegistry stores (strategy_id → expected_bytecode_hash) and the
//   per-strategy freeze flag.  Both are hot-reloadable (freeze is write-once via
//   governance; bytecode hashes require L3 48h timelock for changes).
//
//   StrategyFreezeGuard wraps IntegrityRegistry and provides the fast-path
//   freeze check used in the blueprint submission hot path (atomic read, no lock).
//
// Freeze semantics (spec S8):
//   Once frozen, a strategy CANNOT be unfrozen programmatically — this matches the
//   Solidity implementation where strategy_frozen is write-once (set to true; no
//   unfreeze function).  Governance can deploy a new strategy with a different ID.
//
// Thread-safety:
//   DashMap provides concurrent read/write.  Freeze is a write-once bool in a
//   DashSet so the "is_frozen" check is a lock-free set membership test.
//
// ## Audit fix (this revision): removed placeholder zero-filled strategy entries
//
// `default_strategy_entries()` previously hardcoded `bytecode_hash: [0u8; 32]` and
// `contract_address: [0u8; 20]` for all five strategies, with a comment admitting
// these were placeholders "replaced with real deployed hashes during the pre-audit
// deployment step." Shipping that function meant the integrity registry could be
// populated with zero-value entries by default — either causing `check_bytecode`
// to reject every real blueprint (since a real deployed contract's bytecode hash
// is never `[0u8; 32]`), or, if some other part of the pipeline also happened to
// default `strategy_bytecode_hash` to zero, silently PASSING an integrity check
// that verified nothing. Neither outcome belongs in this file with no loud
// failure attached.
//
// Replaced with `strategy_entries_from_manifest()`, which takes real deployment
// data (hex-encoded bytecode hash / contract address per strategy, as would be
// read from a deployment artifacts file per spec Section 21.3) and:
//   - rejects malformed hex outright,
//   - rejects the wrong byte length outright,
//   - rejects an all-zero hash or address outright, with an error message that
//     names the placeholder problem explicitly, not just "invalid input" —
//     so this exact class of bug fails loudly at startup instead of silently
//     shipping. There is no code path left in this file that can produce a
//     zero-filled `StrategyEntry`.
//
// This does not invent a deployment-artifact file format or loader — that's a
// decision for whatever code constructs a `DeploymentManifest` (out of scope for
// this file, and not something this crate has visibility into). This file only
// guarantees that whatever manifest is supplied cannot contain placeholder data
// without being rejected.

use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::SecurityError;
use crate::metrics;
use crate::signer::keccak256;

// ─── Strategy entry ───────────────────────────────────────────────────────────

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

// ─── Integrity registry ───────────────────────────────────────────────────────

/// Strategy integrity registry.
///
/// `Arc<IntegrityRegistry>` is shared between the blueprint submission path,
/// the control-plane governance handler, and the health FSM.
pub struct IntegrityRegistry {
    /// strategy_id → StrategyEntry (hot-swappable on L3 deployment).
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

    // ── Registration ──────────────────────────────────────────────────────────

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

    // ── Bytecode integrity check (Certora C4) ─────────────────────────────────

    /// Verify that `actual_bytecode_hash` matches the registered expected hash
    /// for `strategy_id`.
    ///
    /// In production, `actual_bytecode_hash` is computed by the orchestrator
    /// as `keccak256(abi.encodePacked(stratAddr.codehash))` at execution time.
    /// The Rust simulation path calls this before submitting to relays.
    ///
    /// Returns:
    ///   `Ok(())`                     — hash matches; safe to execute.
    ///   `Err(BytecodeMismatch)`      — hash differs; HALT-worthy.
    ///   `Err(StrategyUnknown)`       — strategy not in registry.
    ///   `Err(StrategyFrozen)`        — strategy frozen; must be caught by freeze check first.
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

    // ── Freeze management (Certora C7) ────────────────────────────────────────

    /// Freeze a strategy.
    ///
    /// Write-once: once frozen, cannot be unfrozen (spec C7 / Orchestrator.freezeStrategy).
    /// Requires DEFAULT_ADMIN_ROLE in the Orchestrator; here called by the L2
    /// governance handler after a signed freeze proposal.
    pub fn freeze(&self, strategy_id: &str) {
        self.frozen.insert(strategy_id.to_string());
        metrics::STRATEGY_FREEZES.inc();
        tracing::warn!(strategy = strategy_id, "strategy FROZEN — no further blueprints permitted");
    }

    /// True if the strategy is frozen.
    ///
    /// Hot path: DashSet membership test is O(1) and lock-free.
    #[inline]
    pub fn is_frozen(&self, strategy_id: &str) -> bool {
        self.frozen.contains(strategy_id)
    }

    /// Check freeze status — returns `Err(StrategyFrozen)` if frozen.
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
        self.check_frozen(strategy_id)?;     // C7 — cheap; first
        self.check_bytecode(strategy_id, actual_bytecode_hash)?; // C4 — lookup
        Ok(())
    }

    // ── Observability ─────────────────────────────────────────────────────────

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

// ─── StrategyFreezeGuard ─────────────────────────────────────────────────────

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

// ─── Deployment manifest → StrategyEntry ──────────────────────────────────────

/// One strategy's deployment record, as read from a real deployment artifacts
/// file (spec Section 21.3 — "pre-audit deployment step"). Hex fields carry an
/// optional `0x` prefix.
///
/// This type carries no defaults and no `Default` impl on purpose: every field
/// must come from an actual deployment, not from a struct literal with zeroed
/// placeholders sitting in source control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyDeployment {
    /// Strategy identifier string (e.g., "SA", "LA", "MEV", "CNRY", "MSA").
    pub strategy_id: String,
    /// Hex-encoded keccak256 hash of the deployed strategy contract's runtime
    /// bytecode (32 bytes / 64 hex chars, optional `0x` prefix).
    pub bytecode_hash: String,
    /// Hex-encoded deployed contract address (20 bytes / 40 hex chars,
    /// optional `0x` prefix).
    pub contract_address: String,
    /// Phase at which this strategy becomes active.
    pub min_phase: u8,
}

/// A full deployment manifest — one entry per strategy contract actually
/// deployed on-chain. Loaded from disk by the caller (out of scope for this
/// crate) and passed to `strategy_entries_from_manifest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentManifest {
    pub strategies: Vec<StrategyDeployment>,
}

/// Parse a hex-encoded 32-byte bytecode hash, rejecting anything that isn't
/// exactly 32 real, non-zero bytes.
///
/// The all-zero rejection is deliberate and load-bearing: a real deployed
/// contract's keccak256 bytecode hash is never `[0u8; 32]` (keccak256 of any
/// non-empty input is never the zero digest in practice for real bytecode),
/// so an all-zero value here can only mean "this manifest entry still has
/// placeholder data" — exactly the bug this function exists to make
/// impossible to ship silently.
fn parse_bytecode_hash(hex_str: &str, strategy_id: &str) -> Result<[u8; 32], SecurityError> {
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).map_err(|e| {
        SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: format!("bytecode_hash is not valid hex: {e}"),
        }
    })?;
    if bytes.len() != 32 {
        return Err(SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: format!("bytecode_hash must be exactly 32 bytes, got {}", bytes.len()),
        });
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    if arr == [0u8; 32] {
        return Err(SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: "bytecode_hash is all-zero — this is placeholder data, not a real \
                      deployed bytecode hash; refusing to register a strategy with no \
                      actual integrity check behind it".to_string(),
        });
    }
    Ok(arr)
}

/// Parse a hex-encoded 20-byte contract address, rejecting anything that
/// isn't exactly 20 real, non-zero bytes. See `parse_bytecode_hash` for why
/// the all-zero rejection is deliberate, not incidental.
fn parse_contract_address(hex_str: &str, strategy_id: &str) -> Result<[u8; 20], SecurityError> {
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).map_err(|e| {
        SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: format!("contract_address is not valid hex: {e}"),
        }
    })?;
    if bytes.len() != 20 {
        return Err(SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: format!("contract_address must be exactly 20 bytes, got {}", bytes.len()),
        });
    }
    let mut arr = [0u8; 20];
    arr.copy_from_slice(&bytes);
    if arr == [0u8; 20] {
        return Err(SecurityError::InvalidDeploymentEntry {
            strategy_id: strategy_id.to_string(),
            detail: "contract_address is all-zero — this is placeholder data, not a real \
                      deployed contract address; refusing to register".to_string(),
        });
    }
    Ok(arr)
}

/// Build the list of `StrategyEntry` to register at startup (or after an L3
/// deployment) from a real `DeploymentManifest`, filtered to strategies whose
/// `min_phase <= phase`.
///
/// Every entry is validated: malformed hex, wrong byte length, or an
/// all-zero hash/address is rejected with `SecurityError::InvalidDeploymentEntry`
/// rather than silently producing a zero-filled `StrategyEntry`. There is no
/// fallback path in this function that returns placeholder data — a manifest
/// with a bad entry fails the whole call, on purpose, since a partially-loaded
/// integrity registry (some strategies checked, one silently unchecked) is
/// worse than refusing to start.
pub fn strategy_entries_from_manifest(
    manifest: &DeploymentManifest,
    phase:    u8,
) -> Result<Vec<StrategyEntry>, SecurityError> {
    manifest
        .strategies
        .iter()
        .filter(|dep| dep.min_phase <= phase)
        .map(|dep| {
            let bytecode_hash    = parse_bytecode_hash(&dep.bytecode_hash, &dep.strategy_id)?;
            let contract_address = parse_contract_address(&dep.contract_address, &dep.strategy_id)?;
            Ok(StrategyEntry {
                strategy_id: dep.strategy_id.clone(),
                bytecode_hash,
                contract_address,
                min_phase: dep.min_phase,
            })
        })
        .collect()
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

    // ── Bytecode check (C4) ───────────────────────────────────────────────────

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

    // ── Freeze (C7) ───────────────────────────────────────────────────────────

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
        // Freeze again (idempotent) — still frozen
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

    // ── Full integrity check ──────────────────────────────────────────────────

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

    // ── StrategyFreezeGuard ───────────────────────────────────────────────────

    #[test]
    fn freeze_guard_reflects_registry_state() {
        let reg   = reg_with_sa();
        let guard = StrategyFreezeGuard::new(Arc::clone(&reg));
        assert!(!guard.is_frozen("SA"));
        reg.freeze("SA");
        assert!(guard.is_frozen("SA"));
    }

    // ── Observability ─────────────────────────────────────────────────────────

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

    // ── strategy_entries_from_manifest: valid manifests ───────────────────────

    fn sample_manifest() -> DeploymentManifest {
        DeploymentManifest {
            strategies: vec![
                StrategyDeployment {
                    strategy_id:      "CNRY".into(),
                    bytecode_hash:    format!("0x{}", "11".repeat(32)),
                    contract_address: format!("0x{}", "21".repeat(20)),
                    min_phase:        0,
                },
                StrategyDeployment {
                    strategy_id:      "SA".into(),
                    bytecode_hash:    format!("0x{}", "12".repeat(32)),
                    contract_address: format!("0x{}", "22".repeat(20)),
                    min_phase:        1,
                },
                StrategyDeployment {
                    strategy_id:      "MSA".into(),
                    bytecode_hash:    format!("0x{}", "13".repeat(32)),
                    contract_address: format!("0x{}", "23".repeat(20)),
                    min_phase:        2,
                },
                StrategyDeployment {
                    strategy_id:      "LA".into(),
                    bytecode_hash:    format!("0x{}", "14".repeat(32)),
                    contract_address: format!("0x{}", "24".repeat(20)),
                    min_phase:        3,
                },
                StrategyDeployment {
                    strategy_id:      "MEV".into(),
                    bytecode_hash:    format!("0x{}", "15".repeat(32)),
                    contract_address: format!("0x{}", "25".repeat(20)),
                    min_phase:        4,
                },
            ],
        }
    }

    #[test]
    fn manifest_phase0_includes_cnry_only() {
        let entries = strategy_entries_from_manifest(&sample_manifest(), 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].strategy_id, "CNRY");
        assert_ne!(entries[0].bytecode_hash, [0u8; 32]);
        assert_ne!(entries[0].contract_address, [0u8; 20]);
    }

    #[test]
    fn manifest_phase4_includes_all_strategies() {
        let entries = strategy_entries_from_manifest(&sample_manifest(), 4).unwrap();
        assert_eq!(entries.len(), 5);
        for e in &entries {
            assert_ne!(e.bytecode_hash, [0u8; 32], "no entry may carry a placeholder hash");
            assert_ne!(e.contract_address, [0u8; 20], "no entry may carry a placeholder address");
        }
    }

    #[test]
    fn manifest_hex_prefix_is_optional() {
        let mut m = sample_manifest();
        m.strategies[0].bytecode_hash    = "11".repeat(32); // no 0x prefix
        m.strategies[0].contract_address = "21".repeat(20);
        let entries = strategy_entries_from_manifest(&m, 0).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn manifest_entries_round_trip_into_working_integrity_check() {
        let entries = strategy_entries_from_manifest(&sample_manifest(), 4).unwrap();
        let reg = IntegrityRegistry::new();
        reg.register_all(entries);
        let sa = sample_manifest().strategies.into_iter().find(|s| s.strategy_id == "SA").unwrap();
        let hash = parse_bytecode_hash(&sa.bytecode_hash, "SA").unwrap();
        assert!(reg.full_integrity_check("SA", &hash).is_ok());
    }

    // ── strategy_entries_from_manifest: rejection of placeholder/bad data ─────

    #[test]
    fn manifest_rejects_all_zero_bytecode_hash() {
        let mut m = sample_manifest();
        m.strategies[0].bytecode_hash = format!("0x{}", "00".repeat(32));
        let result = strategy_entries_from_manifest(&m, 0);
        assert!(
            matches!(result, Err(SecurityError::InvalidDeploymentEntry { .. })),
            "an all-zero bytecode_hash must be rejected as placeholder data, not silently accepted"
        );
    }

    #[test]
    fn manifest_rejects_all_zero_contract_address() {
        let mut m = sample_manifest();
        m.strategies[0].contract_address = format!("0x{}", "00".repeat(20));
        let result = strategy_entries_from_manifest(&m, 0);
        assert!(matches!(result, Err(SecurityError::InvalidDeploymentEntry { .. })));
    }

    #[test]
    fn manifest_rejects_malformed_hex() {
        let mut m = sample_manifest();
        m.strategies[0].bytecode_hash = "not-hex-at-all".into();
        let result = strategy_entries_from_manifest(&m, 0);
        assert!(matches!(result, Err(SecurityError::InvalidDeploymentEntry { .. })));
    }

    #[test]
    fn manifest_rejects_wrong_length_hash() {
        let mut m = sample_manifest();
        m.strategies[0].bytecode_hash = format!("0x{}", "11".repeat(16)); // 16 bytes, not 32
        let result = strategy_entries_from_manifest(&m, 0);
        assert!(matches!(result, Err(SecurityError::InvalidDeploymentEntry { .. })));
    }

    #[test]
    fn manifest_rejects_wrong_length_address() {
        let mut m = sample_manifest();
        m.strategies[0].contract_address = format!("0x{}", "21".repeat(10)); // 10 bytes, not 20
        let result = strategy_entries_from_manifest(&m, 0);
        assert!(matches!(result, Err(SecurityError::InvalidDeploymentEntry { .. })));
    }

    #[test]
    fn manifest_one_bad_entry_fails_the_whole_call() {
        // A manifest with one good entry and one placeholder entry must not
        // silently register the good one and drop the bad one — that would
        // leave the integrity registry partially populated with no signal
        // that a strategy went unchecked. The whole call fails instead.
        let mut m = sample_manifest();
        m.strategies[1].bytecode_hash = format!("0x{}", "00".repeat(32)); // SA now placeholder
        let result = strategy_entries_from_manifest(&m, 4);
        assert!(result.is_err());
    }

    #[test]
    fn empty_manifest_produces_empty_entries_not_an_error() {
        // An intentionally empty manifest (e.g. a fresh testnet with nothing
        // deployed yet) is valid input, distinct from a manifest containing
        // placeholder entries — this must succeed with zero entries, not fail.
        let m = DeploymentManifest { strategies: vec![] };
        let entries = strategy_entries_from_manifest(&m, 4).unwrap();
        assert!(entries.is_empty());
    }
}