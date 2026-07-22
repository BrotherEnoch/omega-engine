// crates/omega-health/src/reorg_handler.rs
//
// LA Blueprint Reorg Guard â€” spec Â§11.4.
//
// During a chain reorganisation, an LA blueprint in the 'submitted' state
// may reference a block that gets orphaned.  If the referenced block is
// orphaned, the blueprint will not execute â€” the position monitor must
// re-score the position after the chain stabilises.
//
// ## Detection
//
//   The reorg guard watches block hash changes from the RPC layer
//   (omega-rpc).  When a block at height H is reported with a different
//   hash than previously seen, all blueprints submitted AT block H are
//   moved to `ReorgRisk` state.
//
// ## Protection invariant (Â§11.3 interaction)
//
//   The `submitted_positions` deduplication guard (SequencerRestartHandler
//   in omega-strategies) is NOT cleared on reorg detection.  This
//   prevents re-submitting to the same position on a reorganised chain
//   before the 60-block restart window expires.
//
// ## Recovery
//
//   Each blueprint in `ReorgRisk` is re-scored after 5 blocks (the chain
//   stability window).  Re-scoring is scheduled by emitting a
//   `ReorgRiskEvent` on the public channel; the LA strategy task
//   consumes these and re-triggers position evaluation.
//
// ## Data structures
//
//   `ReorgGuard` is a shared, thread-safe struct held in an `Arc`.
//   It is updated by the block-watching task and queried by the LA
//   blueprint submission path.
//
//   `SubmittedBlueprint` tracks (tx_hash, submitted_at_block) pairs.
//   The map is pruned every block â€” entries older than STABILITY_WINDOW
//   blocks are removed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy_primitives::{TxHash, B256};
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;

/// Number of blocks to wait after reorg detection before re-scoring
/// (the chain stability window).  Spec Â§11.4.
pub const STABILITY_WINDOW_BLOCKS: u64 = 5;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ReorgRiskEvent
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Emitted when a submitted blueprint's block is orphaned.
///
/// Consumed by the LA strategy task to trigger position re-scoring.
#[derive(Debug, Clone)]
pub struct ReorgRiskEvent {
    /// Transaction hash of the submitted blueprint.
    pub tx_hash:        TxHash,
    /// Block that was orphaned.
    pub orphaned_block: u64,
    /// Block at which re-scoring should be attempted.
    pub rescore_at:     u64,
    /// UTC timestamp of the reorg detection.
    pub detected_at:    DateTime<Utc>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SubmittedBlueprint
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A blueprint that has been submitted to a relay, tracked for reorg risk.
#[derive(Debug, Clone)]
struct SubmittedBlueprint {
    /// Hash of the block the blueprint was submitted at.
    /// Stored for cross-validation: if we see a new block at the same height
    /// with a different hash, this field is compared to confirm the reorg.
    /// Read path is inside `on_new_block` via `known_blocks` rather than
    /// directly on this field â€” suppressed until that read is wired in.
    #[allow(dead_code)]
    block_hash:   B256,
    /// Block number the blueprint was submitted at.
    submitted_at: u64,
    /// Whether this blueprint has been moved to ReorgRisk state.
    reorg_risk:   bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ReorgGuard
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// LA blueprint reorg guard (spec Â§11.4).
///
/// Shared via `Arc<ReorgGuard>` between:
///   - The block-watching task (calls `on_new_block`)
///   - The LA blueprint submission path (calls `track_submission`)
///   - The LA strategy task (subscribes to `reorg_events()`)
#[derive(Debug)]
pub struct ReorgGuard {
    inner:    Mutex<ReorgGuardInner>,
    /// Broadcast channel for ReorgRiskEvents.  Capacity 64 â€” a reorg
    /// large enough to produce more than 64 simultaneous blueprint
    /// invalidations is an extraordinary event; lagging receivers will
    /// miss events and must reconcile via the full position rescan.
    event_tx: broadcast::Sender<ReorgRiskEvent>,
}

#[derive(Debug)]
struct ReorgGuardInner {
    /// Known block hashes: block_number â†’ block_hash.
    /// Used to detect when a block's hash changes (reorg signal).
    known_blocks:  HashMap<u64, B256>,
    /// Submitted blueprints: tx_hash â†’ SubmittedBlueprint.
    submitted:     HashMap<TxHash, SubmittedBlueprint>,
    /// Current confirmed block number.
    current_block: u64,
}

impl ReorgGuard {
    /// Create a new reorg guard.
    pub fn new() -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(64);
        Arc::new(Self {
            inner: Mutex::new(ReorgGuardInner {
                known_blocks:  HashMap::new(),
                submitted:     HashMap::new(),
                current_block: 0,
            }),
            event_tx,
        })
    }

    /// Subscribe to reorg risk events.
    ///
    /// Returns a `broadcast::Receiver` that yields a `ReorgRiskEvent`
    /// for every blueprint moved to ReorgRisk state.
    pub fn reorg_events(&self) -> broadcast::Receiver<ReorgRiskEvent> {
        self.event_tx.subscribe()
    }

    /// Called by the block-watching task when a new block is observed.
    ///
    /// If `block_hash` differs from the previously recorded hash for
    /// `block_number`, a reorg is detected and all submitted blueprints
    /// AT that block are moved to ReorgRisk.
    pub fn on_new_block(&self, block_number: u64, block_hash: B256) {
        let mut inner = self.inner.lock().expect("reorg guard mutex poisoned");
        inner.current_block = block_number;

        // Prune submitted blueprints older than STABILITY_WINDOW + slack
        let prune_before = block_number.saturating_sub(STABILITY_WINDOW_BLOCKS + 10);
        inner.submitted.retain(|_, bp| bp.submitted_at >= prune_before);
        inner.known_blocks.retain(|&num, _| num >= prune_before);

        // Check for reorg: do we already know this block number with a
        // different hash?
        if let Some(&known_hash) = inner.known_blocks.get(&block_number) {
            if known_hash != block_hash {
                // Reorg detected at block_number
                tracing::warn!(
                    block_number,
                    old_hash = %known_hash,
                    new_hash = %block_hash,
                    "Chain reorg detected â€” scanning for orphaned blueprints",
                );

                let rescore_at     = block_number + STABILITY_WINDOW_BLOCKS;
                let detected_at    = chrono::Utc::now();
                let mut to_publish = Vec::new();

                for (tx_hash, bp) in inner.submitted.iter_mut() {
                    if bp.submitted_at == block_number && !bp.reorg_risk {
                        bp.reorg_risk = true;
                        to_publish.push(ReorgRiskEvent {
                            tx_hash:        *tx_hash,
                            orphaned_block: block_number,
                            rescore_at,
                            detected_at,
                        });
                    }
                }

                // Release lock before broadcasting
                drop(inner);

                for event in to_publish {
                    tracing::warn!(
                        tx_hash        = %event.tx_hash,
                        orphaned_block = event.orphaned_block,
                        rescore_at     = event.rescore_at,
                        "Blueprint moved to REORG_RISK",
                    );
                    // Ignore send error â€” no active subscribers is fine
                    // (engine may be in startup or shutdown).
                    let _ = self.event_tx.send(event);
                }
                return;
            }
        }

        // No reorg â€” record/update the block hash
        inner.known_blocks.insert(block_number, block_hash);
    }

    /// Track a newly submitted blueprint.
    ///
    /// Called by the LA submission path immediately after the bundle is
    /// sent to a relay.
    pub fn track_submission(
        &self,
        tx_hash:      TxHash,
        block_number: u64,
        block_hash:   B256,
    ) {
        let mut inner = self.inner.lock().expect("reorg guard mutex poisoned");
        inner.submitted.insert(tx_hash, SubmittedBlueprint {
            block_hash,
            submitted_at: block_number,
            reorg_risk:   false,
        });
    }

    /// Returns `true` if the blueprint with `tx_hash` has been moved to
    /// ReorgRisk state.
    pub fn is_reorg_risk(&self, tx_hash: &TxHash) -> bool {
        self.inner
            .lock()
            .expect("reorg guard mutex poisoned")
            .submitted
            .get(tx_hash)
            .map(|bp| bp.reorg_risk)
            .unwrap_or(false)
    }

    /// Current confirmed block number as last reported to `on_new_block`.
    pub fn current_block(&self) -> u64 {
        self.inner.lock().expect("reorg guard mutex poisoned").current_block
    }
}

impl Default for ReorgGuard {
    fn default() -> Self {
        // Arc::new is called in new(); this impl is for owned (non-Arc) use in tests
        let (event_tx, _) = broadcast::channel(64);
        Self {
            inner: Mutex::new(ReorgGuardInner {
                known_blocks:  HashMap::new(),
                submitted:     HashMap::new(),
                current_block: 0,
            }),
            event_tx,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Tests
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> B256 {
        B256::from([n; 32])
    }

    fn tx(n: u8) -> TxHash {
        TxHash::from([n; 32])
    }

    #[test]
    fn no_reorg_on_first_block() {
        let g = ReorgGuard::default();
        let mut rx = g.reorg_events();
        g.on_new_block(100, hash(1));
        // No event should be queued
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn same_hash_does_not_trigger_reorg() {
        let g = ReorgGuard::default();
        let mut rx = g.reorg_events();
        g.on_new_block(100, hash(1));
        g.on_new_block(100, hash(1)); // same hash â€” no reorg
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn different_hash_triggers_reorg_and_marks_blueprints() {
        let g = ReorgGuard::default();
        let mut rx = g.reorg_events();

        // Submit a blueprint at block 100 with hash(1)
        g.track_submission(tx(0xAA), 100, hash(1));
        g.on_new_block(100, hash(1));   // first observation â€” no reorg
        g.on_new_block(100, hash(2));   // different hash â€” reorg!

        let event = rx.try_recv().expect("reorg event must be emitted");
        assert_eq!(event.orphaned_block, 100);
        assert_eq!(event.tx_hash, tx(0xAA));
        assert_eq!(event.rescore_at, 100 + STABILITY_WINDOW_BLOCKS);
        assert!(g.is_reorg_risk(&tx(0xAA)));
    }

    #[test]
    fn blueprint_at_different_block_not_marked() {
        let g = ReorgGuard::default();
        // Submit blueprint at block 99
        g.track_submission(tx(0xBB), 99, hash(1));
        g.on_new_block(100, hash(1));
        // Reorg at block 100 â€” blueprint at 99 must not be marked
        g.on_new_block(100, hash(2));
        assert!(!g.is_reorg_risk(&tx(0xBB)));
    }

    #[test]
    fn old_entries_pruned() {
        let g = ReorgGuard::default();
        g.track_submission(tx(0xCC), 1, hash(1));
        // Advance past the prune window
        let far_ahead = 1 + STABILITY_WINDOW_BLOCKS + 20;
        g.on_new_block(far_ahead, hash(9));
        // Entry should be pruned â€” is_reorg_risk returns false for
        // unknown tx_hash
        assert!(!g.is_reorg_risk(&tx(0xCC)));
    }
}