// crates/omega-relay/src/dedup.rs
//! Sequencer restart double-spend protection (Â§11.3, C3).
//!
//! Tracks positions submitted during a sequencer restart window via a
//! `DashMap<PositionKey, submission_block>` with 60-block auto-expiry.
//! `try_submit` guarantees exactly-once submission per position per restart
//! window across all relay channels.
//!
//! ## Spec
//! - DashSet semantics (DashMap used for block-timestamp expiry)
//! - 60-block expiry window (~15 s on Arbitrum at 250 ms/block)
//! - `try_submit` returns `true` only on first call for a position
//! - `on_new_block` prunes expired entries (called once per block)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tracing::debug;

use crate::error::{RelayError, RelayResult};

// â”€â”€ Types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Stable key identifying a liquidation position.
/// Opaque bytes â€” callers construct from protocol + account address + asset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionKey(pub [u8; 32]);

impl PositionKey {
    /// Construct from arbitrary bytes via keccak256-style truncation.
    pub fn from_bytes(b: &[u8]) -> Self {
        use sha3::{Digest, Keccak256};
        let hash = Keccak256::digest(b);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash);
        Self(arr)
    }

    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }
}

// â”€â”€ SequencerRestartHandler â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Restart window in blocks.  60 blocks â‰ˆ 15 s on Arbitrum (250 ms/block).
pub const RESTART_WINDOW_BLOCKS: u64 = 60;

/// Thread-safe double-spend guard for sequencer restart windows.
pub struct SequencerRestartHandler {
    /// `PositionKey` â†’ block number of first submission.
    submitted_positions: Arc<DashMap<PositionKey, u64>>,
    /// Block number at which the current restart window began.
    restart_block: AtomicU64,
    /// Configurable window length (default: `RESTART_WINDOW_BLOCKS`).
    restart_window_blocks: u64,
}

impl SequencerRestartHandler {
    pub fn new(restart_block: u64) -> Arc<Self> {
        Arc::new(Self {
            submitted_positions: Arc::new(DashMap::new()),
            restart_block: AtomicU64::new(restart_block),
            restart_window_blocks: RESTART_WINDOW_BLOCKS,
        })
    }

    pub fn with_window(restart_block: u64, window_blocks: u64) -> Arc<Self> {
        Arc::new(Self {
            submitted_positions: Arc::new(DashMap::new()),
            restart_block: AtomicU64::new(restart_block),
            restart_window_blocks: window_blocks,
        })
    }

    /// Called once per new block.  Prunes entries older than
    /// `restart_window_blocks` so the map does not grow unboundedly.
    pub fn on_new_block(&self, block: u64) {
        let threshold = block.saturating_sub(self.restart_window_blocks);
        let before = self.submitted_positions.len();
        self.submitted_positions
            .retain(|_, &mut submission_block| submission_block >= threshold);
        let pruned = before - self.submitted_positions.len();
        if pruned > 0 {
            debug!(block, pruned, "dedup: pruned expired position entries");
        }
    }

    /// Attempt to claim exclusive submission rights for `position` at `current_block`.
    ///
    /// Returns `Ok(true)` â€” first caller, proceed with submission.
    /// Returns `Err(RelayError::DuplicateSubmission)` â€” position already claimed.
    pub fn try_submit(&self, position: &PositionKey, current_block: u64) -> RelayResult<bool> {
        use dashmap::mapref::entry::Entry;
        match self.submitted_positions.entry(position.clone()) {
            Entry::Vacant(e) => {
                e.insert(current_block);
                Ok(true)
            }
            Entry::Occupied(e) => Err(RelayError::DuplicateSubmission {
                position_key: position.as_hex(),
                submitted_block: *e.get(),
            }),
        }
    }

    /// Whether `position` has already been submitted in the current window.
    #[inline]
    pub fn is_submitted(&self, position: &PositionKey) -> bool {
        self.submitted_positions.contains_key(position)
    }

    /// Current size of the dedup map (for observability).
    #[inline]
    pub fn pending_count(&self) -> usize {
        self.submitted_positions.len()
    }

    /// Update the restart block (called when sequencer restart is detected).
    pub fn mark_restart(&self, block: u64) {
        self.restart_block.store(block, Ordering::SeqCst);
        // Clear all entries â€” the restart represents a new dedup epoch.
        self.submitted_positions.clear();
        debug!(block, "dedup: sequencer restart marked, dedup map cleared");
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(s: &str) -> PositionKey {
        PositionKey::from_bytes(s.as_bytes())
    }

    #[test]
    fn first_submit_returns_true() {
        let h = SequencerRestartHandler::new(100);
        assert!(h.try_submit(&pos("aave:0xabc:usdc"), 100).unwrap());
    }

    #[test]
    fn second_submit_same_position_errors() {
        let h = SequencerRestartHandler::new(100);
        h.try_submit(&pos("aave:0xabc:usdc"), 100).unwrap();
        let result = h.try_submit(&pos("aave:0xabc:usdc"), 101);
        assert!(
            matches!(result, Err(RelayError::DuplicateSubmission { .. })),
            "second submit must be DuplicateSubmission"
        );
    }

    #[test]
    fn different_positions_are_independent() {
        let h = SequencerRestartHandler::new(100);
        assert!(h.try_submit(&pos("aave:0xabc:usdc"), 100).unwrap());
        assert!(h.try_submit(&pos("compound:0xdef:weth"), 100).unwrap());
    }

    #[test]
    fn on_new_block_prunes_expired_entries() {
        let h = SequencerRestartHandler::with_window(0, 5);
        h.try_submit(&pos("pos1"), 10).unwrap();
        h.try_submit(&pos("pos2"), 11).unwrap();

        // Block 70: threshold = 70 - 5 = 65. Both entries (10, 11) expired.
        h.on_new_block(70);
        assert_eq!(h.pending_count(), 0, "both entries must be pruned");

        // Same positions should now be submittable again.
        assert!(h.try_submit(&pos("pos1"), 70).unwrap());
    }

    #[test]
    fn recent_entries_survive_pruning() {
        let h = SequencerRestartHandler::with_window(0, 60);
        h.try_submit(&pos("pos_old"), 1).unwrap();
        h.try_submit(&pos("pos_new"), 60).unwrap();

        // Block 61: threshold = 61 - 60 = 1. pos_old (block 1) survives (>= 1).
        h.on_new_block(61);
        assert_eq!(h.pending_count(), 2, "neither entry expired at block 61");

        // Block 62: threshold = 2. pos_old (block 1) < 2 â†’ pruned.
        h.on_new_block(62);
        assert_eq!(h.pending_count(), 1, "pos_old must be pruned at block 62");
    }

    #[test]
    fn mark_restart_clears_all_entries() {
        let h = SequencerRestartHandler::new(100);
        for i in 0u8..10 {
            h.try_submit(&PositionKey([i; 32]), 100).unwrap();
        }
        assert_eq!(h.pending_count(), 10);
        h.mark_restart(200);
        assert_eq!(h.pending_count(), 0, "restart must clear all entries");

        // All positions should be submittable again after restart.
        assert!(h.try_submit(&PositionKey([0u8; 32]), 200).unwrap());
    }

    #[test]
    fn concurrent_submit_race_only_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let h = Arc::new(SequencerRestartHandler::new(0).as_ref().clone());
        let wins = Arc::new(AtomicUsize::new(0));
        let h = SequencerRestartHandler::new(0);

        let winner = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let h = Arc::clone(&h);
                let w = Arc::clone(&winner);
                std::thread::spawn(move || {
                    if h.try_submit(&pos("race_position"), 1).is_ok() {
                        w.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = i;
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            winner.load(Ordering::Relaxed),
            1,
            "exactly one thread must win the race"
        );
    }
}