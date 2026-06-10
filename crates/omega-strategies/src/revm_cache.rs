// crates/omega-strategies/src/revm_cache.rs
//
// EIL double-buffer revm state cache (spec §6).
//
// ## Problem
//
//   A single shared revm state can race: a writer updating account slots
//   while a reader builds a blueprint leads to partial-update exposure
//   (the reader sees some old slots and some new ones).  This produces
//   SIMULATION_STATE_MISMATCH (§13.4) losses.
//
// ## Solution: double-buffer + atomic pointer flip
//
//   Two caches (`cache_a`, `cache_b`) alternate as active/inactive.
//   The writer always populates the INACTIVE cache, then atomically
//   swaps the active pointer.  Readers always see a fully-committed
//   snapshot.  No locking required on the read path.
//
//   Update SLA: < 50ms from new block arrival to cache ready (§6).
//   At 250ms Arbitrum block times this gives ≥ 4 full update cycles
//   before the next block.
//
// ## Staleness guard (§13.4 SIMULATION_STATE_MISMATCH prevention)
//
//   The spec §13.4 fix M3 requires the revm trust window to be reduced
//   from 2 blocks to 1 block after a SIMULATION_STATE_MISMATCH event.
//   `RevmStateCache::is_stale` enforces this by comparing against the
//   current block number and the configured trust window.
//
// ## Usage
//
//   ```rust
//   // In the update task (runs every block):
//   cache_manager.update(new_block_number).await;
//
//   // In the simulation path:
//   let snap = cache_manager.current();
//   if snap.is_stale(signal.block_number, trust_window_blocks) {
//       return Err(OmegaError::dropped(DropCode::SimulationStateMismatch));
//   }
//   ```

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;

use arc_swap::ArcSwap;

// ─────────────────────────────────────────────────────────────────────────────
// RevmStateCache
// ─────────────────────────────────────────────────────────────────────────────

/// A single committed snapshot of on-chain state for revm simulation.
///
/// Immutable after construction.  Shared via `Arc` — cloning is O(1).
///
/// ## Contents
///
/// In the full implementation this struct holds a `revm::db::CacheDB`
/// or equivalent in-memory EVM state.  In omega-strategies the concrete
/// state is represented by the metadata fields below; the actual account
/// map is populated by omega-oracle and injected via `RevmCacheManager::update_with`.
#[derive(Debug)]
pub struct RevmStateCache {
    /// Block number this snapshot was taken at.
    pub block_number: u64,

    /// Wall-clock time the snapshot was committed.
    /// Used for latency metrics (§16) — not for staleness checks.
    pub committed_at: Instant,

    /// EIP-1559 base fee at this block (gwei).
    /// Copied from the oracle FeeSnapshot so simulation can use it
    /// without re-fetching.
    pub base_fee_gwei: u64,

    /// Number of account slots loaded into this snapshot.
    /// Exposed for observability / dashboard.
    pub slot_count: usize,
}

impl RevmStateCache {
    /// Create a new snapshot for `block_number`.
    pub fn new(block_number: u64, base_fee_gwei: u64, slot_count: usize) -> Arc<Self> {
        Arc::new(Self {
            block_number,
            committed_at: Instant::now(),
            base_fee_gwei,
            slot_count,
        })
    }

    /// Returns `true` when this snapshot is too old to trust for simulation.
    ///
    /// `current_block` is the latest block reported by the oracle layer.
    /// `trust_window` is the maximum number of blocks the snapshot remains
    /// valid for (default 1 after a SIMULATION_STATE_MISMATCH event per
    /// §13.4; default 2 in normal operation).
    ///
    /// A snapshot at block 100 with trust_window=1 is stale at block 102.
    #[inline]
    pub fn is_stale(&self, current_block: u64, trust_window: u64) -> bool {
        current_block > self.block_number + trust_window
    }

    /// Milliseconds elapsed since this snapshot was committed.
    pub fn age_ms(&self) -> u64 {
        self.committed_at.elapsed().as_millis() as u64
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RevmCacheManager
// ─────────────────────────────────────────────────────────────────────────────

/// Double-buffer EIL revm state cache manager (§6).
///
/// Thread-safe — `Arc<RevmCacheManager>` is shared between the oracle
/// update task (writer) and any number of simulation tasks (readers).
///
/// ## Initialisation
///
/// Both cache buffers start with a sentinel block_number = 0.  The
/// `current()` reader checks staleness before use, so an uninitialised
/// cache produces a `is_stale` result until the first `update` call.
pub struct RevmCacheManager {
    /// 0 = cache_a is active; 1 = cache_b is active.
    active: AtomicUsize,
    cache_a: ArcSwap<RevmStateCache>,
    cache_b: ArcSwap<RevmStateCache>,

    /// Staleness trust window in blocks.
    /// Reduced from 2 → 1 after SIMULATION_STATE_MISMATCH (§13.4).
    trust_window: AtomicUsize,
}

impl RevmCacheManager {
    /// Construct a new manager.
    ///
    /// `trust_window_blocks`: maximum blocks a snapshot remains valid for.
    /// Use `2` for normal operation; `1` after SIMULATION_STATE_MISMATCH.
    pub fn new(trust_window_blocks: u64) -> Arc<Self> {
        // Sentinel snapshots at block 0 — will be stale immediately.
        let sentinel = RevmStateCache::new(0, 0, 0);
        Arc::new(Self {
            active: AtomicUsize::new(0),
            cache_a: ArcSwap::from(Arc::clone(&sentinel)),
            cache_b: ArcSwap::from(sentinel),
            trust_window: AtomicUsize::new(trust_window_blocks as usize),
        })
    }

    /// Return the current active snapshot.
    ///
    /// O(1) — atomic load + Arc clone.  Never blocks.
    /// Callers must check `is_stale` before using the snapshot.
    #[inline]
    pub fn current(&self) -> Arc<RevmStateCache> {
        match self.active.load(Ordering::Acquire) {
            0 => self.cache_a.load_full(),
            _ => self.cache_b.load_full(),
        }
    }

    /// Commit a new cache snapshot for `block_number`.
    ///
    /// ## Write sequence
    ///
    /// 1. Identify the INACTIVE buffer.
    /// 2. Store the new snapshot into it (ArcSwap — non-blocking).
    /// 3. Atomically flip the `active` index.
    ///
    /// After step 3, all new readers see the updated snapshot.
    /// Readers already holding the old snapshot (from before step 3)
    /// continue safely — the old Arc is still live.
    ///
    /// The comment "Update SLA: < 50ms" applies to the caller's
    /// responsibility to invoke this method promptly after receiving
    /// the block event.
    pub fn update(&self, block_number: u64, base_fee_gwei: u64, slot_count: usize) {
        let current_active = self.active.load(Ordering::Acquire);
        let inactive = 1 - current_active;

        let new_snap = RevmStateCache::new(block_number, base_fee_gwei, slot_count);

        // Write to the inactive buffer
        match inactive {
            0 => self.cache_a.store(new_snap),
            _ => self.cache_b.store(new_snap),
        }

        // Atomic flip — all new readers see the new snapshot from here
        self.active.store(inactive, Ordering::Release);

        tracing::debug!(
            block_number,
            base_fee_gwei,
            slot_count,
            inactive_buf = inactive,
            "RevmCacheManager: snapshot committed",
        );
    }

    /// Read the current trust window in blocks.
    #[inline]
    pub fn trust_window(&self) -> u64 {
        self.trust_window.load(Ordering::Relaxed) as u64
    }

    /// Reduce the trust window to 1 block after a SIMULATION_STATE_MISMATCH
    /// event (§13.4, fix M3 corrective action).
    pub fn tighten_trust_window(&self) {
        self.trust_window.store(1, Ordering::Relaxed);
        tracing::warn!(
            "RevmCacheManager: trust window tightened to 1 block \
             (SIMULATION_STATE_MISMATCH corrective action §13.4)",
        );
    }

    /// Reset the trust window to the normal value (2 blocks).
    ///
    /// Called after the tightened window has been in effect for a full
    /// epoch without further mismatches.
    pub fn reset_trust_window(&self) {
        self.trust_window.store(2, Ordering::Relaxed);
        tracing::info!("RevmCacheManager: trust window restored to 2 blocks");
    }

    /// Returns `true` when the current snapshot is stale for the
    /// given block number.
    pub fn is_stale(&self, current_block: u64) -> bool {
        let snap = self.current();
        snap.is_stale(current_block, self.trust_window())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_is_stale_before_first_update() {
        let mgr = RevmCacheManager::new(2);
        // Block 0 sentinel; any block > 0 + 2 = 2 is stale
        assert!(mgr.is_stale(3), "uninitialised cache must be stale");
    }

    #[test]
    fn update_makes_current_snapshot_non_stale() {
        let mgr = RevmCacheManager::new(2);
        mgr.update(100, 10, 500);
        // Block 100, trust=2 → stale at 103+
        assert!(!mgr.is_stale(101));
        assert!(!mgr.is_stale(102));
        assert!(mgr.is_stale(103));
    }

    #[test]
    fn double_buffer_flip_is_atomic() {
        let mgr = RevmCacheManager::new(2);
        mgr.update(10, 5, 100);
        assert_eq!(mgr.current().block_number, 10);
        mgr.update(11, 6, 110);
        assert_eq!(mgr.current().block_number, 11);
        mgr.update(12, 7, 120);
        assert_eq!(mgr.current().block_number, 12);
    }

    #[test]
    fn alternating_buffers() {
        let mgr = RevmCacheManager::new(2);
        // First update writes to buffer 1 (inactive when active=0), flips to 1
        mgr.update(1, 0, 0);
        assert_eq!(mgr.active.load(Ordering::Relaxed), 1);
        // Second update writes to buffer 0, flips to 0
        mgr.update(2, 0, 0);
        assert_eq!(mgr.active.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn tighten_trust_window() {
        let mgr = RevmCacheManager::new(2);
        mgr.update(100, 10, 0);
        // Normal: stale at 103
        assert!(!mgr.is_stale(102));
        // Tighten to 1 block
        mgr.tighten_trust_window();
        assert_eq!(mgr.trust_window(), 1);
        // Now stale at 102
        assert!(mgr.is_stale(102));
        assert!(!mgr.is_stale(101));
    }

    #[test]
    fn reset_trust_window() {
        let mgr = RevmCacheManager::new(2);
        mgr.tighten_trust_window();
        mgr.reset_trust_window();
        assert_eq!(mgr.trust_window(), 2);
    }

    #[test]
    fn staleness_exact_boundary() {
        // is_stale: current_block > block_number + trust_window
        let snap = RevmStateCache::new(50, 5, 0);
        assert!(!snap.is_stale(52, 2)); // 52 > 52 → false
        assert!(snap.is_stale(53, 2)); // 53 > 52 → true
    }
}
