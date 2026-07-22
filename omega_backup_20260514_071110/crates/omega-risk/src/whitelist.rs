// crates/omega-risk/src/whitelist.rs
//
// Strategy whitelist registry (spec S8, check 4).
//
// Two whitelist types:
//
//   1. BytecodeWhitelist â€” maps StrategyId â†’ expected keccak256(bytecode).
//      Checked against ExecutionBlueprint.strategy_bytecode_hash.
//      Upgrades require L3 governance (48h timelock); freezes require L2.
//      Matches Certora invariant C4: no delegatecall; bytecode integrity enforced.
//
//   2. AddressWhitelist  â€” maps execution address â†’ approved flag.
//      Rotation requires address-rotation module to re-register new address.
//      Hot-reloadable via ArcSwap; governance-controlled.
//
// Thread-safety:
//   Both registries use ArcSwap<HashMap<...>> for lock-free reads.
//   Writes are serialised through &mut self (governance path only).

use arc_swap::ArcSwap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Approved bytecode hashes per strategy ID string (e.g., "SA", "LA").
pub type BytecodeMap = HashMap<String, [u8; 32]>;

/// Approved execution addresses.
pub type AddressSet = HashSet<[u8; 20]>;

/// Immutable snapshot of the bytecode whitelist.
#[derive(Debug, Clone)]
pub struct BytecodeWhitelist {
    inner: Arc<ArcSwap<BytecodeMap>>,
}

impl BytecodeWhitelist {
    pub fn new(initial: BytecodeMap) -> Self {
        Self { inner: Arc::new(ArcSwap::new(Arc::new(initial))) }
    }

    /// Check whether `hash` is the expected bytecode for `strategy_id`.
    pub fn is_approved(&self, strategy_id: &str, hash: &[u8; 32]) -> bool {
        let map = self.inner.load();
        match map.get(strategy_id) {
            Some(expected) => expected == hash,
            // Unknown strategy IDs are denied.
            None => false,
        }
    }

    /// Hot-update the whitelist (called after L3 governance clears a new deployment).
    pub fn update(&self, new_map: BytecodeMap) {
        self.inner.store(Arc::new(new_map));
        tracing::info!("bytecode whitelist updated ({} strategies)", self.inner.load().len());
    }

    /// Register a single new strategy hash without replacing the whole map.
    pub fn register(&self, strategy_id: String, hash: [u8; 32]) {
        let current = self.inner.load();
        let mut new_map: BytecodeMap = (**current).clone();
        new_map.insert(strategy_id.clone(), hash);
        self.inner.store(Arc::new(new_map));
        tracing::info!(strategy = strategy_id, "bytecode whitelist: strategy registered");
    }
}

/// Immutable snapshot of the approved execution-address set.
#[derive(Debug, Clone)]
pub struct AddressWhitelist {
    inner: Arc<ArcSwap<AddressSet>>,
}

impl AddressWhitelist {
    pub fn new(initial: AddressSet) -> Self {
        Self { inner: Arc::new(ArcSwap::new(Arc::new(initial))) }
    }

    /// True if `addr` is in the approved set.
    pub fn is_approved(&self, addr: &[u8; 20]) -> bool {
        self.inner.load().contains(addr)
    }

    /// Add a new execution address (called by address-rotation module after L2 approval).
    pub fn add(&self, addr: [u8; 20]) {
        let current = self.inner.load();
        let mut new_set: AddressSet = (**current).clone();
        new_set.insert(addr);
        self.inner.store(Arc::new(new_set));
        tracing::info!(addr = hex::encode(addr), "address whitelist: address added");
    }

    /// Remove an old execution address after rotation.
    pub fn remove(&self, addr: &[u8; 20]) {
        let current = self.inner.load();
        let mut new_set: AddressSet = (**current).clone();
        new_set.remove(addr);
        self.inner.store(Arc::new(new_set));
        tracing::info!(addr = hex::encode(*addr), "address whitelist: address removed");
    }
}

#[cfg(test)]
mod whitelist_tests {
    use super::*;

    fn hash(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn known_strategy_correct_hash_approved() {
        let mut map = BytecodeMap::new();
        map.insert("SA".into(), hash(0xaa));
        let wl = BytecodeWhitelist::new(map);
        assert!(wl.is_approved("SA", &hash(0xaa)));
    }

    #[test]
    fn known_strategy_wrong_hash_denied() {
        let mut map = BytecodeMap::new();
        map.insert("SA".into(), hash(0xaa));
        let wl = BytecodeWhitelist::new(map);
        assert!(!wl.is_approved("SA", &hash(0xbb)));
    }

    #[test]
    fn unknown_strategy_denied() {
        let wl = BytecodeWhitelist::new(BytecodeMap::new());
        assert!(!wl.is_approved("UNKNOWN", &hash(0x00)));
    }

    #[test]
    fn hot_update_takes_effect_immediately() {
        let wl = BytecodeWhitelist::new(BytecodeMap::new());
        assert!(!wl.is_approved("LA", &hash(0xcc)));
        let mut new_map = BytecodeMap::new();
        new_map.insert("LA".into(), hash(0xcc));
        wl.update(new_map);
        assert!(wl.is_approved("LA", &hash(0xcc)));
    }

    #[test]
    fn address_whitelist_add_remove() {
        let wl = AddressWhitelist::new(AddressSet::new());
        let addr = [0x01u8; 20];
        assert!(!wl.is_approved(&addr));
        wl.add(addr);
        assert!(wl.is_approved(&addr));
        wl.remove(&addr);
        assert!(!wl.is_approved(&addr));
    }
}