// crates/omega-security/src/replay.rs
//
// Chain-scoped nonce registry + blueprint replay guard (spec S3 / Certora C5).
//
// Spec references:
//   OmegaOrchestrator.sol:
//     mapping(bytes32 => bool)   public executed_blueprints;  // replay protection
//     mapping(bytes32 => uint64) public next_nonce;           // chain-scoped per strategy
//
//   ExecutionBlueprint.nonce_key():
//     keccak256(strategy_id_hash || chain_id.to_be_bytes())
//
//   Certora C5 — Replay impossibility:
//     "After setting executed, cannot execute again"
//
// Design:
//   ReplayGuard tracks executed blueprint hashes in a DashSet (lock-free concurrent reads).
//   NonceRegistry tracks the expected next nonce per (strategy_id, chain_id) key.
//
//   Both are append-only from the security layer's perspective — hashes and nonces
//   are added after on-chain confirmation is observed.  The sequencer-restart
//   guard in omega-strategies handles the shorter-lived in-flight dedup window.
//
// Persistence:
//   The executed set is kept in memory + optionally flushed to disk on shutdown.
//   On startup the set is rehydrated from the on-chain state (replay via RPC).
//   The nonce registry is always sourced from the chain via on_chain_nonce_sync().
//
// Thread-safety:
//   DashMap/DashSet provide lock-free reads and fine-grained shard locks for writes.
//   All public methods take &self.
//
// ## Audit fix (this revision): nonce_map_key did not actually mirror nonce_key()
//
// This file's own doc comment (above, and previously on `nonce_map_key` itself)
// asserted that the local key derivation "mirrors `ExecutionBlueprint::nonce_key()`
// in omega-core." It did not. `ExecutionBlueprint::nonce_key()` is a two-stage
// hash — `keccak256(keccak256(strategy_id_string) || chain_id_be_bytes)` — chosen
// specifically (per that function's own comment) so the strategy discriminant is
// hashed on its own before being combined with `chain_id`. `nonce_map_key` was
// instead hashing `strategy_id_bytes || chain_id_bytes` in a single pass — a
// different key for the same logical `(strategy_id, chain_id)` pair.
//
// This was not a live functional bug: `NonceRegistry` only ever compares its own
// derivation against itself (`validate`/`advance`/`on_chain_nonce_sync` all route
// through the same `nonce_map_key`), so internal self-consistency held. But the
// doc-asserted equivalence with `omega-core`'s hash was false, and any future code
// that trusted the comment to cross-reference a nonce key between the two crates
// (e.g. to look up a `NonceState` using a key derived from
// `ExecutionBlueprint::nonce_key()` directly) would have silently gotten a
// different key and missed the entry. Fixed to use the same two-stage
// keccak256(keccak256(..) || ..) structure, and a regression test now calls
// `omega_core::types::blueprint::ExecutionBlueprint::nonce_key()` directly and
// asserts byte-for-byte equality, so this can't drift silently again — `Cargo.toml`
// already depends on `omega-core`, so this cross-check costs nothing new.

use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::SecurityError;
use crate::metrics;

// ─── Blueprint replay guard ────────────────────────────────────────────────────

/// Chain-scoped key: (blueprint_hash[32], chain_id[8]) → 40 bytes, used as DashSet key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReplayKey {
    hash: [u8; 32],
    chain_id: u64,
}

/// Set of executed blueprint hashes — prevents replay across sequencer restarts.
///
/// Certora C5: once a hash is in this set, no blueprint with that hash can execute again.
#[derive(Clone)]
pub struct ReplayGuard {
    executed: Arc<DashSet<ReplayKey>>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self {
            executed: Arc::new(DashSet::new()),
        }
    }

    /// Check whether `blueprint_hash` has already been executed on `chain_id`.
    ///
    /// Returns `Err(ReplayDetected)` if the hash is already in the set.
    /// Returns `Ok(())` otherwise (does NOT insert — call `mark_executed` after
    /// on-chain confirmation).
    pub fn check(&self, blueprint_hash: &[u8; 32], chain_id: u64) -> Result<(), SecurityError> {
        let key = ReplayKey {
            hash: *blueprint_hash,
            chain_id,
        };
        if self.executed.contains(&key) {
            metrics::REPLAY_ATTEMPTS.inc();
            tracing::error!(
                hash = hex::encode(blueprint_hash),
                chain_id,
                "REPLAY DETECTED — blueprint already executed"
            );
            return Err(SecurityError::ReplayDetected {
                hash: hex::encode(blueprint_hash),
                chain_id,
            });
        }
        Ok(())
    }

    /// Mark a blueprint as executed after on-chain confirmation.
    ///
    /// Idempotent: inserting a duplicate is silently ignored (DashSet semantics).
    pub fn mark_executed(&self, blueprint_hash: &[u8; 32], chain_id: u64) {
        let key = ReplayKey {
            hash: *blueprint_hash,
            chain_id,
        };
        self.executed.insert(key);
        tracing::debug!(
            hash = hex::encode(blueprint_hash),
            chain_id,
            "blueprint marked executed in replay guard"
        );
    }

    /// Check-and-mark atomically using a DashSet entry API.
    ///
    /// Returns `Err(ReplayDetected)` if already present; inserts and returns `Ok(())`
    /// if this is the first time.  Used by the submission path to guarantee
    /// exactly-once-per-blueprint semantics across all relay channels.
    pub fn check_and_mark(
        &self,
        blueprint_hash: &[u8; 32],
        chain_id: u64,
    ) -> Result<(), SecurityError> {
        let key = ReplayKey {
            hash: *blueprint_hash,
            chain_id,
        };
        // DashSet::insert returns true if the value was newly inserted.
        if !self.executed.insert(key) {
            metrics::REPLAY_ATTEMPTS.inc();
            return Err(SecurityError::ReplayDetected {
                hash: hex::encode(blueprint_hash),
                chain_id,
            });
        }
        Ok(())
    }

    /// Number of executed blueprints tracked (for observability).
    pub fn len(&self) -> usize {
        self.executed.len()
    }

    /// True when the guard has no entries (fresh start or after replay).
    pub fn is_empty(&self) -> bool {
        self.executed.is_empty()
    }

    /// Seed from a list of already-executed hashes loaded from the chain on startup.
    pub fn seed_from_chain(&self, hashes: impl IntoIterator<Item = ([u8; 32], u64)>) {
        let mut count = 0usize;
        for (hash, chain_id) in hashes {
            self.executed.insert(ReplayKey { hash, chain_id });
            count += 1;
        }
        tracing::info!(count, "replay guard seeded from chain state");
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Nonce registry ───────────────────────────────────────────────────────────

/// Chain-scoped nonce key: keccak256(keccak256(strategy_id) || chain_id).
///
/// Must exactly mirror `ExecutionBlueprint::nonce_key()` in omega-core — see this
/// file's audit note above. The strategy discriminant is hashed on its own first
/// (`inner`), then combined with `chain_id` in a second hash, rather than hashing
/// both inputs together in one pass; the two-stage structure is what
/// `ExecutionBlueprint::nonce_key()` actually does, so this must match it exactly,
/// not just produce "a" deterministic key.
fn nonce_map_key(strategy_id: &str, chain_id: u64) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let inner: [u8; 32] = Keccak256::digest(strategy_id.as_bytes()).into();
    let mut h = Keccak256::new();
    h.update(inner);
    h.update(chain_id.to_be_bytes());
    h.finalize().into()
}

/// Per-(strategy, chain) nonce state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonceState {
    /// Next expected nonce (matches on-chain `next_nonce[key]`).
    pub next_nonce: u64,
    /// Chain ID this nonce is scoped to.
    pub chain_id: u64,
    /// Strategy ID string.
    pub strategy_id: String,
}

/// Chain-scoped nonce registry.
///
/// Enforces monotone nonce progression per (strategy_id, chain_id) pair.
/// The authoritative source of truth is always the on-chain mapping;
/// this registry is a local cache that is synced via `on_chain_nonce_sync()`.
#[derive(Clone)]
pub struct NonceRegistry {
    nonces: Arc<DashMap<[u8; 32], NonceState>>,
}

impl NonceRegistry {
    pub fn new() -> Self {
        Self {
            nonces: Arc::new(DashMap::new()),
        }
    }

    /// Validate that `blueprint_nonce` matches the expected next nonce for
    /// `(strategy_id, chain_id)`.
    ///
    /// Does NOT advance the nonce — call `advance()` after successful on-chain inclusion.
    pub fn validate(
        &self,
        strategy_id: &str,
        chain_id: u64,
        blueprint_nonce: u64,
    ) -> Result<(), SecurityError> {
        let key = nonce_map_key(strategy_id, chain_id);
        match self.nonces.get(&key) {
            Some(state) => {
                if blueprint_nonce != state.next_nonce {
                    return Err(SecurityError::NonceMismatch {
                        strategy_id: strategy_id.to_string(),
                        chain_id,
                        expected: state.next_nonce,
                        got: blueprint_nonce,
                    });
                }
            }
            None => {
                // First blueprint for this (strategy, chain) — nonce must be 0.
                if blueprint_nonce != 0 {
                    return Err(SecurityError::NonceMismatch {
                        strategy_id: strategy_id.to_string(),
                        chain_id,
                        expected: 0,
                        got: blueprint_nonce,
                    });
                }
            }
        }
        Ok(())
    }

    /// Advance the nonce for `(strategy_id, chain_id)` after on-chain confirmation.
    pub fn advance(&self, strategy_id: &str, chain_id: u64) -> Result<u64, SecurityError> {
        let key = nonce_map_key(strategy_id, chain_id);
        let mut entry = self.nonces.entry(key).or_insert_with(|| NonceState {
            next_nonce: 0,
            chain_id,
            strategy_id: strategy_id.to_string(),
        });

        let current = entry.next_nonce;
        let next = current
            .checked_add(1)
            .ok_or_else(|| SecurityError::NonceOverflow {
                strategy_id: strategy_id.to_string(),
            })?;
        entry.next_nonce = next;
        Ok(next)
    }

    /// Sync the expected nonce from the on-chain value (called on startup and
    /// after sequencer restart).
    pub fn on_chain_nonce_sync(&self, strategy_id: &str, chain_id: u64, chain_nonce: u64) {
        let key = nonce_map_key(strategy_id, chain_id);
        self.nonces.insert(
            key,
            NonceState {
                next_nonce: chain_nonce,
                chain_id,
                strategy_id: strategy_id.to_string(),
            },
        );
        tracing::debug!(
            strategy = strategy_id,
            chain_id,
            chain_nonce,
            "nonce synced from chain"
        );
    }

    /// Return the expected next nonce for `(strategy_id, chain_id)`, or 0 if unknown.
    pub fn next_nonce(&self, strategy_id: &str, chain_id: u64) -> u64 {
        let key = nonce_map_key(strategy_id, chain_id);
        self.nonces.get(&key).map(|s| s.next_nonce).unwrap_or(0)
    }

    /// Snapshot all nonce states (for persistence / observability).
    pub fn snapshot(&self) -> Vec<NonceState> {
        self.nonces.iter().map(|e| e.value().clone()).collect()
    }
}

impl Default for NonceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;

    // ── ReplayGuard ───────────────────────────────────────────────────────────

    #[test]
    fn fresh_hash_passes_check() {
        let g = ReplayGuard::new();
        assert!(g.check(&[0x01; 32], 42161).is_ok());
    }

    #[test]
    fn executed_hash_fails_check() {
        let g = ReplayGuard::new();
        g.mark_executed(&[0x01; 32], 42161);
        assert!(matches!(
            g.check(&[0x01; 32], 42161),
            Err(SecurityError::ReplayDetected { .. })
        ));
    }

    #[test]
    fn same_hash_different_chain_is_allowed() {
        let g = ReplayGuard::new();
        g.mark_executed(&[0xaa; 32], 42161);
        assert!(g.check(&[0xaa; 32], 1).is_ok()); // Ethereum mainnet is different
    }

    #[test]
    fn check_and_mark_idempotent_second_call_fails() {
        let g = ReplayGuard::new();
        assert!(g.check_and_mark(&[0x05; 32], 42161).is_ok());
        assert!(matches!(
            g.check_and_mark(&[0x05; 32], 42161),
            Err(SecurityError::ReplayDetected { .. })
        ));
    }

    #[test]
    fn seed_from_chain_populates_guard() {
        let g = ReplayGuard::new();
        g.seed_from_chain([([0xbb; 32], 42161), ([0xcc; 32], 42161)]);
        assert!(g.check(&[0xbb; 32], 42161).is_err());
        assert!(g.check(&[0xcc; 32], 42161).is_err());
        assert!(g.check(&[0xdd; 32], 42161).is_ok());
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn concurrent_check_and_mark_only_one_succeeds() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let g = Arc::new(ReplayGuard::new());
        let wins = Arc::new(Mutex::new(0u32));
        let hash = [0xfe; 32];

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let g2 = Arc::clone(&g);
                let w2 = Arc::clone(&wins);
                thread::spawn(move || {
                    if g2.check_and_mark(&hash, 42161).is_ok() {
                        *w2.lock().unwrap() += 1;
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*wins.lock().unwrap(), 1, "exactly one thread must win");
    }

    // ── NonceRegistry ─────────────────────────────────────────────────────────

    #[test]
    fn first_nonce_must_be_zero() {
        let r = NonceRegistry::new();
        assert!(r.validate("SA", 42161, 0).is_ok());
        assert!(r.validate("SA", 42161, 1).is_err());
    }

    #[test]
    fn advance_increments_next_nonce() {
        let r = NonceRegistry::new();
        r.validate("LA", 42161, 0).unwrap();
        r.advance("LA", 42161).unwrap();
        assert!(r.validate("LA", 42161, 1).is_ok());
        assert!(r.validate("LA", 42161, 0).is_err());
    }

    #[test]
    fn nonces_are_chain_scoped() {
        let r = NonceRegistry::new();
        r.advance("SA", 42161).unwrap();
        r.advance("SA", 42161).unwrap();
        // Arbitrum at nonce 2, but Ethereum still at 0.
        assert_eq!(r.next_nonce("SA", 42161), 2);
        assert_eq!(r.next_nonce("SA", 1), 0);
    }

    #[test]
    fn on_chain_sync_overwrites_local() {
        let r = NonceRegistry::new();
        r.advance("MSA", 42161).unwrap();
        assert_eq!(r.next_nonce("MSA", 42161), 1);
        r.on_chain_nonce_sync("MSA", 42161, 99);
        assert_eq!(r.next_nonce("MSA", 42161), 99);
    }

    #[test]
    fn nonce_map_key_is_deterministic() {
        let k1 = nonce_map_key("SA", 42161);
        let k2 = nonce_map_key("SA", 42161);
        assert_eq!(k1, k2);
        let k3 = nonce_map_key("LA", 42161);
        assert_ne!(k1, k3);
    }

    // ── Regression: nonce_map_key must match omega-core's nonce_key() exactly ──

    #[test]
    fn nonce_map_key_matches_execution_blueprint_nonce_key() {
        // Direct cross-crate check against the reference implementation —
        // omega-security already depends on omega-core, so this costs
        // nothing new and makes the doc-asserted equivalence enforceable
        // instead of just claimed in a comment. Covers every StrategyId
        // variant, not just one, since the string discriminant differs
        // per variant (SA/CNRY/MSA/LA/MEV) and each is a separate input
        // to the inner hash.
        use omega_core::types::blueprint::{ExecutionBlueprint, StrategyId};

        let cases = [
            (StrategyId::Sa, "SA"),
            (StrategyId::Cnry, "CNRY"),
            (StrategyId::Msa, "MSA"),
            (StrategyId::La, "LA"),
            (StrategyId::Mev, "MEV"),
        ];

        for (strategy_id, label) in cases {
            for chain_id in [1u64, 42161u64] {
                let expected: [u8; 32] = ExecutionBlueprint::nonce_key(strategy_id, chain_id).0;
                let actual = nonce_map_key(label, chain_id);
                assert_eq!(
                    actual, expected,
                    "nonce_map_key({label:?}, {chain_id}) must exactly match \
                     ExecutionBlueprint::nonce_key({strategy_id:?}, {chain_id})"
                );
            }
        }
    }

    #[test]
    fn nonce_map_key_is_not_a_single_pass_hash() {
        // Regression guard for the specific bug fixed in this revision:
        // a naive single-pass keccak256(strategy_id_bytes || chain_id_bytes)
        // must NOT equal the real two-stage key. If this test starts
        // failing, nonce_map_key has regressed back to the single-pass
        // form that silently diverged from ExecutionBlueprint::nonce_key().
        use sha3::{Digest, Keccak256};

        let strategy_id = "SA";
        let chain_id = 42161u64;

        let mut naive = Keccak256::new();
        naive.update(strategy_id.as_bytes());
        naive.update(chain_id.to_be_bytes());
        let naive_key: [u8; 32] = naive.finalize().into();

        let correct_key = nonce_map_key(strategy_id, chain_id);

        assert_ne!(
            naive_key, correct_key,
            "nonce_map_key must NOT match the naive single-pass hash — \
             the two-stage inner/outer hash must produce a different key"
        );
    }
}
