// crates/omega-execution/src/idempotency.rs
//
// Submission-layer idempotency dedup, keyed on
// ExecutionBlueprint::idempotency_key. Distinct from
// omega_relay::dedup::SequencerRestartHandler (keyed on PositionKey, a
// liquidation position identity, and living inside omega-relay) — see
// ExecutionPipelineSpecification.md's Stage 3 for why this needs to be a
// SEPARATE cache at this layer, checked before a BundlePayload is ever
// built.
//
// Uses the same atomic Entry-match pattern already fixed into
// omega_relay::dedup::SequencerRestartHandler::try_submit and
// omega_risk::kill_switch::KillSwitchRegistry::get_or_create elsewhere in
// this codebase, for the same TOCTOU reason: a separate contains_key
// check followed by a separate insert would let two racing blueprints
// with the same idempotency_key both pass the check.

use alloy_primitives::B256;
use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use std::sync::Arc;

use crate::error::ExecutionError;

#[derive(Clone)]
pub struct IdempotencyCache {
    seen: Arc<DashMap<B256, DateTime<Utc>>>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self { seen: Arc::new(DashMap::new()) }
    }

    /// Atomically checks and marks `key` as seen. Returns
    /// `Err(DuplicateIdempotencyKey)` if this key was already recorded.
    pub fn check_and_mark(&self, key: B256) -> Result<(), ExecutionError> {
        match self.seen.entry(key) {
            Entry::Occupied(_) => Err(ExecutionError::DuplicateIdempotencyKey),
            Entry::Vacant(e) => {
                e.insert(Utc::now());
                Ok(())
            }
        }
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Evict entries older than `max_age`, so this cache doesn't grow
    /// unbounded across a long-running process. Mirrors the eviction
    /// pattern already used by `omega_risk::kill_switch`'s window_history
    /// and `omega_relay::dedup::SequencerRestartHandler::on_new_block`.
    pub fn evict_older_than(&self, max_age: chrono::Duration, now: DateTime<Utc>) {
        self.seen.retain(|_, seen_at| now.signed_duration_since(*seen_at) <= max_age);
    }
}

impl Default for IdempotencyCache {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_check_passes() {
        let cache = IdempotencyCache::new();
        assert!(cache.check_and_mark(B256::from([1u8; 32])).is_ok());
    }

    #[test]
    fn duplicate_check_fails() {
        let cache = IdempotencyCache::new();
        let key = B256::from([2u8; 32]);
        cache.check_and_mark(key).unwrap();
        assert!(matches!(
            cache.check_and_mark(key),
            Err(ExecutionError::DuplicateIdempotencyKey)
        ));
    }

    #[test]
    fn different_keys_independent() {
        let cache = IdempotencyCache::new();
        assert!(cache.check_and_mark(B256::from([3u8; 32])).is_ok());
        assert!(cache.check_and_mark(B256::from([4u8; 32])).is_ok());
    }

    #[test]
    fn concurrent_check_and_mark_only_one_winner() {
        use std::thread;
        let cache = IdempotencyCache::new();
        let key = B256::from([5u8; 32]);
        let wins = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let c = cache.clone();
                let w = std::sync::Arc::clone(&wins);
                thread::spawn(move || {
                    if c.check_and_mark(key).is_ok() {
                        w.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles { h.join().unwrap(); }
        assert_eq!(wins.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn eviction_removes_old_entries() {
        let cache = IdempotencyCache::new();
        let key = B256::from([6u8; 32]);
        let t0 = Utc::now();
        cache.seen.insert(key, t0);
        cache.evict_older_than(chrono::Duration::seconds(60), t0 + chrono::Duration::seconds(120));
        assert!(cache.is_empty());
    }

    #[test]
    fn eviction_keeps_recent_entries() {
        let cache = IdempotencyCache::new();
        let key = B256::from([7u8; 32]);
        let t0 = Utc::now();
        cache.seen.insert(key, t0);
        cache.evict_older_than(chrono::Duration::seconds(60), t0 + chrono::Duration::seconds(30));
        assert_eq!(cache.len(), 1);
    }
}