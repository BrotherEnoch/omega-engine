// crates/omega-relay/src/dedup.rs
//! Sequencer restart double-spend protection (§11.3, C3).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tracing::debug;

use crate::error::{RelayError, RelayResult};

/// Stable key identifying a liquidation position.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionKey(pub [u8; 32]);

impl PositionKey {
    /// Construct from arbitrary bytes via keccak256 hashing.
    pub fn from_bytes(b: &[u8]) -> Self {
        use sha3::{Digest, Keccak256};
        let hash = Keccak256::digest(b);
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash);
        Self(arr)
    }

    /// Render the position key as a lowercase hex string without a `0x` prefix.
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// Restart window in blocks. 60 blocks ≈ 15 s on Arbitrum (250 ms/block).
pub const RESTART_WINDOW_BLOCKS: u64 = 60;

/// Thread-safe double-spend guard for sequencer restart windows (§11.3).
pub struct SequencerRestartHandler {
    submitted_positions:   Arc<DashMap<PositionKey, u64>>,
    restart_block:         AtomicU64,
    restart_window_blocks: u64,
}

impl SequencerRestartHandler {
    /// Create a handler using the default 60-block restart window.
    pub fn new(restart_block: u64) -> Arc<Self> {
        Arc::new(Self {
            submitted_positions:   Arc::new(DashMap::new()),
            restart_block:         AtomicU64::new(restart_block),
            restart_window_blocks: RESTART_WINDOW_BLOCKS,
        })
    }

    /// Create a handler with an explicitly configured restart window length.
    pub fn with_window(restart_block: u64, window_blocks: u64) -> Arc<Self> {
        Arc::new(Self {
            submitted_positions:   Arc::new(DashMap::new()),
            restart_block:         AtomicU64::new(restart_block),
            restart_window_blocks: window_blocks,
        })
    }

    /// Called once per new block. Prunes entries older than `restart_window_blocks`.
    pub fn on_new_block(&self, block: u64) {
        let threshold = block.saturating_sub(self.restart_window_blocks);
        let before    = self.submitted_positions.len();
        self.submitted_positions
            .retain(|_, &mut submission_block| submission_block >= threshold);
        let pruned = before - self.submitted_positions.len();
        if pruned > 0 {
            debug!(block, pruned, "dedup: pruned expired position entries");
        }
    }

    /// Attempt to claim exclusive submission rights for `position` at `current_block`.
    ///
    /// Returns `Ok(true)` on the first call; `Err(DuplicateSubmission)` thereafter.
    pub fn try_submit(&self, position: &PositionKey, current_block: u64) -> RelayResult<bool> {
        use dashmap::mapref::entry::Entry;
        match self.submitted_positions.entry(position.clone()) {
            Entry::Vacant(e) => {
                e.insert(current_block);
                Ok(true)
            }
            Entry::Occupied(e) => Err(RelayError::DuplicateSubmission {
                position_key:    position.as_hex(),
                submitted_block: *e.get(),
            }),
        }
    }

    #[inline]
    /// Returns `true` if `position` has already been submitted in the current window.
    pub fn is_submitted(&self, position: &PositionKey) -> bool {
        self.submitted_positions.contains_key(position)
    }

    #[inline]
    /// Current size of the dedup map (for observability).
    pub fn pending_count(&self) -> usize {
        self.submitted_positions.len()
    }

    /// Update the restart block and clear all entries, starting a new dedup epoch.
    pub fn mark_restart(&self, block: u64) {
        self.restart_block.store(block, Ordering::SeqCst);
        self.submitted_positions.clear();
        debug!(block, "dedup: sequencer restart marked, dedup map cleared");
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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

        h.on_new_block(70);
        assert_eq!(h.pending_count(), 0, "both entries must be pruned");
        assert!(h.try_submit(&pos("pos1"), 70).unwrap());
    }

    #[test]
    fn recent_entries_survive_pruning() {
        let h = SequencerRestartHandler::with_window(0, 60);
        h.try_submit(&pos("pos_old"), 1).unwrap();
        h.try_submit(&pos("pos_new"), 60).unwrap();

        h.on_new_block(61);
        assert_eq!(h.pending_count(), 2, "neither entry expired at block 61");

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
        assert!(h.try_submit(&PositionKey([0u8; 32]), 200).unwrap());
    }

    #[test]
    fn concurrent_submit_race_only_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let h      = SequencerRestartHandler::new(0);
        let winner = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let h = Arc::clone(&h);
                let w = Arc::clone(&winner);
                std::thread::spawn(move || {
                    if h.try_submit(&pos("race_position"), 1).is_ok() {
                        w.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();

        for t in threads { t.join().unwrap(); }

        assert_eq!(
            winner.load(Ordering::Relaxed),
            1,
            "exactly one thread must win the race"
        );
    }
}