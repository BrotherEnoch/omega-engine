// crates/omega-relay/src/reorg.rs
//! L2 reorg guard for LA blueprints during the 80 ms submission window (Â§11.4, V4).
//!
//! ## Spec requirements
//! - Watch block hash changes to detect reorgs.
//! - Blueprints in `Submitted` state whose submission block is orphaned â†’
//!   move to `ReorgRisk` state.
//! - The `submitted_positions` dedup guard is NOT cleared on reorg (prevents
//!   re-submission to the same position in a reorganised chain).
//! - Re-score position after 5 blocks (chain stability window).
//! - Emit `LaReorgRisk` event for every affected blueprint.

use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::{RelayError, RelayResult};

// â”€â”€ Types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Ethereum transaction hash â€” 32 bytes, hex-encoded with `0x` prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TxHash(pub String);

/// Chain stability window before rescoring a reorg-risk blueprint (Â§11.4).
pub const STABILITY_WINDOW_BLOCKS: u64 = 5;

/// State of a tracked LA blueprint submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlueprintState {
    /// Bundle submitted to relay(s); awaiting inclusion.
    Submitted { submitted_at_block: u64 },
    /// Submission block was orphaned â€” blueprint will not execute.
    ReorgRisk { orphaned_block: u64 },
    /// Blueprint included and confirmed.
    Included { inclusion_block: u64 },
}

/// Event emitted for every blueprint that enters `ReorgRisk`.
#[derive(Debug, Clone)]
pub struct LaReorgRiskEvent {
    pub tx_hash: TxHash,
    pub orphaned_block: u64,
    /// Block number at which rescoring should be attempted.
    pub rescore_at_block: u64,
}

// â”€â”€ LaReorgGuard â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Tracks submitted LA blueprints and detects reorgs during the LA window.
pub struct LaReorgGuard {
    /// `TxHash` â†’ current `BlueprintState`.
    submitted: DashMap<TxHash, BlueprintState>,
    /// Most recently seen canonical block hashes: block_number â†’ block_hash.
    canonical_hashes: DashMap<u64, [u8; 32]>,
    /// Channel to emit reorg-risk events to the observability layer.
    event_tx: mpsc::UnboundedSender<LaReorgRiskEvent>,
}

impl LaReorgGuard {
    /// Create a new guard.  Returns the guard and the event receiver.
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<LaReorgRiskEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                submitted: DashMap::new(),
                canonical_hashes: DashMap::new(),
                event_tx: tx,
            }),
            rx,
        )
    }

    // â”€â”€ Submission tracking â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Register a blueprint as submitted at `block`.
    pub fn on_submitted(&self, tx_hash: TxHash, block: u64) {
        self.submitted
            .insert(tx_hash.clone(), BlueprintState::Submitted { submitted_at_block: block });
        info!(
            tx_hash = %tx_hash.0,
            block,
            "reorg-guard: blueprint submitted"
        );
    }

    /// Mark a blueprint as included in the canonical chain.
    pub fn on_included(&self, tx_hash: &TxHash, inclusion_block: u64) {
        self.submitted.insert(
            tx_hash.clone(),
            BlueprintState::Included { inclusion_block },
        );
    }

    // â”€â”€ Block / reorg handling â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Called for every new canonical block header.
    /// Records the block hash so we can detect when a previously-seen hash is
    /// replaced (i.e. a reorg occurred at that height).
    pub fn on_new_block(&self, block: u64, block_hash: [u8; 32]) {
        // Detect reorg: if we already have a different hash for this block number,
        // every blueprint submitted at this block is now at risk.
        if let Some(prev_hash) = self.canonical_hashes.get(&block) {
            if *prev_hash != block_hash {
                warn!(
                    block,
                    old_hash = %hex::encode(*prev_hash),
                    new_hash = %hex::encode(block_hash),
                    "reorg-guard: reorg detected at block {block}"
                );
                self.on_reorg(block);
            }
        }
        self.canonical_hashes.insert(block, block_hash);

        // Prune hashes older than 256 blocks (beyond LA relevance).
        let threshold = block.saturating_sub(256);
        self.canonical_hashes.retain(|&b, _| b >= threshold);
    }

    /// A reorg orphaned `orphaned_block`.  Move every blueprint submitted AT
    /// that block into `ReorgRisk` and emit events.
    ///
    /// The dedup guard (`SequencerRestartHandler`) must NOT be cleared here â€”
    /// per spec Â§11.4 the dedup guard remains active to prevent re-submission.
    pub fn on_reorg(&self, orphaned_block: u64) {
        for mut entry in self.submitted.iter_mut() {
            if let BlueprintState::Submitted { submitted_at_block } = *entry.value() {
                if submitted_at_block == orphaned_block {
                    let tx_hash = entry.key().clone();
                    let rescore_at = orphaned_block + STABILITY_WINDOW_BLOCKS;

                    *entry.value_mut() = BlueprintState::ReorgRisk { orphaned_block };

                    let event = LaReorgRiskEvent {
                        tx_hash: tx_hash.clone(),
                        orphaned_block,
                        rescore_at_block: rescore_at,
                    };

                    if self.event_tx.send(event).is_err() {
                        warn!(tx_hash = %tx_hash.0, "reorg-guard: event channel closed");
                    }

                    warn!(
                        tx_hash = %tx_hash.0,
                        orphaned_block,
                        rescore_at,
                        "reorg-guard: blueprint entered ReorgRisk state"
                    );
                }
            }
        }
    }

    // â”€â”€ Accessors â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Current state of a blueprint.
    pub fn state(&self, tx_hash: &TxHash) -> Option<BlueprintState> {
        self.submitted.get(tx_hash).map(|e| e.value().clone())
    }

    /// All blueprints currently in `ReorgRisk` state.
    pub fn reorg_risk_blueprints(&self) -> Vec<(TxHash, u64)> {
        self.submitted
            .iter()
            .filter_map(|e| {
                if let BlueprintState::ReorgRisk { orphaned_block } = *e.value() {
                    Some((e.key().clone(), orphaned_block))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Pending (submitted, not yet included or reorged) blueprint count.
    pub fn pending_count(&self) -> usize {
        self.submitted
            .iter()
            .filter(|e| matches!(e.value(), BlueprintState::Submitted { .. }))
            .count()
    }
}

// â”€â”€ Tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> [u8; 32] {
        [n; 32]
    }

    fn tx(s: &str) -> TxHash {
        TxHash(s.into())
    }

    #[test]
    fn submitted_blueprint_moves_to_reorg_risk_on_orphan() {
        let (guard, _rx) = LaReorgGuard::new();

        guard.on_submitted(tx("0xabc"), 100);
        guard.on_new_block(100, hash(1));

        // Reorg: block 100 gets a different hash
        guard.on_new_block(100, hash(2));

        let state = guard.state(&tx("0xabc")).unwrap();
        assert!(
            matches!(state, BlueprintState::ReorgRisk { orphaned_block: 100 }),
            "blueprint must be ReorgRisk, got {state:?}"
        );
    }

    #[test]
    fn blueprint_at_different_block_not_affected_by_reorg() {
        let (guard, _rx) = LaReorgGuard::new();

        guard.on_submitted(tx("0xsafe"), 99);
        guard.on_submitted(tx("0xrisky"), 100);

        guard.on_new_block(99, hash(1));
        guard.on_new_block(100, hash(1));
        // Reorg only at block 100
        guard.on_new_block(100, hash(2));

        assert!(
            matches!(
                guard.state(&tx("0xsafe")).unwrap(),
                BlueprintState::Submitted { submitted_at_block: 99 }
            ),
            "block-99 blueprint must remain Submitted"
        );
        assert!(
            matches!(
                guard.state(&tx("0xrisky")).unwrap(),
                BlueprintState::ReorgRisk { .. }
            ),
            "block-100 blueprint must be ReorgRisk"
        );
    }

    #[test]
    fn reorg_emits_event_with_correct_rescore_block() {
        let (guard, mut rx) = LaReorgGuard::new();

        guard.on_submitted(tx("0xevt"), 200);
        guard.on_new_block(200, hash(10));
        guard.on_new_block(200, hash(20)); // triggers reorg

        let event = rx.try_recv().expect("reorg event must be emitted");
        assert_eq!(event.tx_hash, tx("0xevt"));
        assert_eq!(event.orphaned_block, 200);
        assert_eq!(
            event.rescore_at_block,
            200 + STABILITY_WINDOW_BLOCKS,
            "rescore must be scheduled 5 blocks after orphan"
        );
    }

    #[test]
    fn included_blueprint_not_moved_to_reorg_risk() {
        let (guard, _rx) = LaReorgGuard::new();

        guard.on_submitted(tx("0xinc"), 50);
        guard.on_included(&tx("0xinc"), 51);

        // Now reorg at block 50
        guard.on_new_block(50, hash(1));
        guard.on_new_block(50, hash(2));

        // Should still be Included â€” only Submitted â†’ ReorgRisk transition exists.
        assert!(
            matches!(
                guard.state(&tx("0xinc")).unwrap(),
                BlueprintState::Included { .. }
            ),
            "included blueprint must not be moved to ReorgRisk"
        );
    }

    #[test]
    fn same_block_hash_does_not_trigger_reorg() {
        let (guard, mut rx) = LaReorgGuard::new();

        guard.on_submitted(tx("0xstable"), 100);
        guard.on_new_block(100, hash(1));
        guard.on_new_block(100, hash(1)); // same hash again â€” NOT a reorg

        assert!(rx.try_recv().is_err(), "no event should be emitted for identical hash");
        assert!(
            matches!(
                guard.state(&tx("0xstable")).unwrap(),
                BlueprintState::Submitted { .. }
            )
        );
    }

    #[test]
    fn stability_window_is_five_blocks() {
        assert_eq!(STABILITY_WINDOW_BLOCKS, 5, "Â§11.4: 5-block chain stability window");
    }
}