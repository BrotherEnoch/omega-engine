// crates/omega-relay/src/lib.rs
//! `omega-relay` — Relay submission layer for OmegaEngine v12.

#![forbid(unsafe_code)]
#![deny(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod backpressure;
pub mod blacklist;
pub mod client;
pub mod config;
pub mod confirmation;
pub mod dedup;
pub mod error;
pub mod metrics;
pub mod reorg;
pub mod reputation;
pub mod signing;

pub use backpressure::{CascadeResult, CascadeSubmitter, RelayOutcome, RelayRateLimiters};
pub use blacklist::BuilderBlacklist;
pub use client::{BundlePayload, RelayClient, SubmissionOutcome};
pub use config::{
    RelayConfig, RelayName, LA_TIE_BAND_FRACTION, WS_RATE_ANONYMOUS, WS_RATE_AUTHENTICATED,
};
pub use confirmation::{ConfirmationResult, InclusionTracker, CONFIRMATION_GRACE_BLOCKS};
pub use dedup::{PositionKey, SequencerRestartHandler, RESTART_WINDOW_BLOCKS};
pub use error::{RelayError, RelayResult};
pub use metrics::{ExecutionAddress, LaRelayMetrics, RelayRateSnapshot};
pub use reorg::{BlueprintState, LaReorgGuard, LaReorgRiskEvent, TxHash, STABILITY_WINDOW_BLOCKS};
pub use reputation::{carryover_pct, rotate_address, shuffled_submission_order, submission_order};
pub use signing::{FlashbotsSigner, RelayAuth};

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

/// Top-level handle that owns all relay clients, metrics, dedup guard,
/// reorg guard, builder blacklist, and inclusion confirmation tracker.
///
/// ## Audit fix (this revision): reorg-risk events were silently discarded
///
/// `LaReorgGuard::new()` returns `(Arc<LaReorgGuard>, mpsc::UnboundedReceiver<
/// LaReorgRiskEvent>)`. `MultiRelayClient::new` previously did
/// `let (reorg_guard, _event_rx) = LaReorgGuard::new();` — dropping the
/// receiver immediately. Once dropped, every subsequent `event_tx.send(...)`
/// inside `LaReorgGuard::on_reorg` fails silently (logged only at `warn!`
/// level as "event channel closed"), meaning no caller of this public
/// constructor could ever observe a `LaReorgRiskEvent` — the blueprint state
/// transition to `ReorgRisk` still happened internally (queryable via
/// `reorg_guard().state()`/`reorg_risk_blueprints()`), but the event stream
/// this type exists to also provide was dead on arrival. Fixed by returning
/// the receiver from `new()` instead of discarding it, so the actual caller
/// — who has visibility into whatever rescoring logic consumes
/// `LaReorgRiskEvent` (not visible from this crate) — can wire it up. This
/// is a breaking signature change to `new()`, the same category as other
/// breaking-but-correct fixes elsewhere in this codebase: the alternative
/// (silently eating the channel) is strictly worse.
pub struct MultiRelayClient {
    submitter: CascadeSubmitter,
    dedup: Arc<SequencerRestartHandler>,
    reorg_guard: Arc<LaReorgGuard>,
    blacklist: Arc<BuilderBlacklist>,
    metrics: Arc<LaRelayMetrics>,
    relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
    rate_limiters: Arc<RelayRateLimiters>,
    inclusion_tracker: Arc<InclusionTracker>,
}

impl MultiRelayClient {
    /// Construct the full relay layer. `cfg.confirmation_rpc_url` must point at a real
    /// chain JSON-RPC endpoint — see `confirmation::InclusionTracker`.
    ///
    /// Returns the client alongside the reorg-risk event receiver — see this
    /// struct's audit note above for why this is no longer discarded
    /// internally. Callers that don't need reorg-risk notifications can
    /// simply drop the receiver themselves; the difference is that the
    /// choice is now theirs to make, not made silently for them.
    pub fn new(
        relay_clients: HashMap<String, Arc<dyn RelayClient>>,
        metrics: Arc<LaRelayMetrics>,
        blacklist: Arc<BuilderBlacklist>,
        cfg: &RelayConfig,
        startup_block: u64,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<LaReorgRiskEvent>) {
        let relay_clients = Arc::new(relay_clients);
        let inclusion_tracker = InclusionTracker::new(cfg.confirmation_rpc_url.clone());
        let submitter = CascadeSubmitter::new(
            Arc::clone(&relay_clients),
            Arc::clone(&metrics),
            cfg,
            Arc::clone(&inclusion_tracker),
        );

        let relay_names: Vec<String> = relay_clients.keys().cloned().collect();
        let rate_limiters = Arc::new(RelayRateLimiters::new(
            &relay_names,
            cfg.max_bundles_per_relay_per_second as u32,
        ));

        let (reorg_guard, event_rx) = LaReorgGuard::new();

        let client = Arc::new(Self {
            submitter,
            dedup: SequencerRestartHandler::new(startup_block),
            reorg_guard,
            blacklist,
            metrics,
            relay_clients,
            rate_limiters,
            inclusion_tracker,
        });

        (client, event_rx)
    }

    /// Submit a bundle in cascade mode.
    pub async fn cascade_submit(&self, bundles: Vec<BundlePayload>) -> Vec<CascadeResult> {
        self.submitter.submit_cascade(bundles).await
    }

    /// Submit a single bundle (non-cascade LA path).
    pub async fn submit_single(&self, bundle: BundlePayload) -> RelayResult<bool> {
        backpressure::submit_single_bundle(
            bundle,
            &self.relay_clients,
            &self.metrics,
            &self.rate_limiters,
            &self.inclusion_tracker,
        )
        .await
    }

    /// Claim submission rights for a position in the restart window.
    pub fn claim_position(&self, position: &PositionKey, current_block: u64) -> RelayResult<bool> {
        self.dedup.try_submit(position, current_block)
    }

    /// Notify dedup and reorg guard of a new block header. Fast and synchronous —
    /// does NOT resolve pending inclusion confirmations; call `reconcile_inclusions`
    /// separately (it needs real network I/O and shouldn't block this).
    pub fn on_new_block(&self, block: u64, block_hash: [u8; 32]) {
        self.dedup.on_new_block(block);
        self.reorg_guard.on_new_block(block, block_hash);
    }

    /// Call once per new canonical block, alongside `on_new_block` — resolves every
    /// bundle that's been waiting on inclusion confirmation past its target block, and
    /// feeds the REAL confirmed result into `LaRelayMetrics`. This is where the
    /// reputation/ranking system actually gets fed accurate data, instead of the old
    /// "relay said 200 OK" signal.
    pub async fn reconcile_inclusions(&self, current_block: u64) -> Vec<ConfirmationResult> {
        let results = self.inclusion_tracker.reconcile(current_block).await;
        for r in &results {
            self.metrics.record(&r.relay, r.included);
        }
        results
    }

    /// Register a submitted blueprint with the reorg guard.
    pub fn on_bundle_submitted(&self, tx_hash: TxHash, block: u64) {
        self.reorg_guard.on_submitted(tx_hash, block);
    }

    /// Mark a sequencer restart, resetting the dedup epoch.
    pub fn on_sequencer_restart(&self, block: u64) {
        self.dedup.mark_restart(block);
    }

    /// Hot-reload the builder blacklist from disk.
    pub fn reload_blacklist(&self) -> RelayResult<usize> {
        self.blacklist.reload()
    }

    /// Check if a builder key is blacklisted (§12.3).
    pub fn is_builder_blacklisted(&self, key: &str) -> bool {
        self.blacklist.contains(key)
    }

    /// Current LA inclusion-rate ranked relay list.
    pub fn ranked_relays(&self) -> Vec<RelayRateSnapshot> {
        self.metrics.la_ranked_relays()
    }

    /// Access shared metrics for address rotation (§14.1).
    pub fn metrics(&self) -> &Arc<LaRelayMetrics> {
        &self.metrics
    }

    /// Access the reorg guard for blueprint status queries.
    pub fn reorg_guard(&self) -> &Arc<LaReorgGuard> {
        &self.reorg_guard
    }

    /// Number of bundles currently awaiting on-chain inclusion confirmation.
    pub fn pending_confirmations(&self) -> usize {
        self.inclusion_tracker.pending_count()
    }
}

// ── Integration tests ─────────────────────────────────────────────────────────

// `clippy::expect_used` is added here alongside the pre-existing
// `unwrap_used`/`indexing_slicing` allows: this crate's `#![deny(...)]` above
// is workspace-wide (including tests, unlike e.g. omega-hot-path's
// `#![cfg_attr(not(test), deny(...))]` split), and `new_returns_a_live_reorg_event_receiver`
// below legitimately needs `.expect("must not time out")` /
// `.expect("channel must not be closed")` to assert on a timeout/recv result
// with a meaningful failure message — the same idiom `unwrap_used` was
// already allowed for in this module.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]
mod integration_tests {
    use super::*;
    use crate::client::MockRelayClient;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_blacklist_file() -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            "[[blacklisted_builders]]\nkey = \"0xbad\"\nreason = \"test\"\nadded = \"2026-04-19\"\n"
        )
        .unwrap();
        f
    }

    /// Test helper — discards the reorg-risk event receiver, since none of
    /// these tests exercise reorg notifications. Production callers should
    /// NOT copy this pattern; see `MultiRelayClient::new`'s audit note on
    /// why silently discarding this receiver was the bug fixed in this
    /// revision. Tests are an intentional exception: nothing here sends a
    /// reorg event, so there's nothing to lose by not consuming it.
    fn make_multi_relay(startup_block: u64) -> Arc<MultiRelayClient> {
        let f = make_blacklist_file();
        let blacklist = BuilderBlacklist::load(f.path()).unwrap();

        let addr = ExecutionAddress("0xINTEGRATION".into());
        let metrics = LaRelayMetrics::new(100, addr.clone());

        for i in 0..20 {
            metrics.record(&RelayName::Flashbots, i < 18);
            metrics.record(&RelayName::Bloxroute, i < 15);
        }

        let mut clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        clients.insert("flashbots".into(), Arc::new(MockRelayClient::new(true)));
        clients.insert("bloxroute".into(), Arc::new(MockRelayClient::new(false)));

        let cfg = RelayConfig {
            stagger_ms: 0,
            max_bundles_per_relay_per_second: 100,
            confirmation_rpc_url: "http://localhost:1".into(),
            ..Default::default()
        };

        let _ = f;
        let (mr, _event_rx) =
            MultiRelayClient::new(clients, metrics, blacklist, &cfg, startup_block);
        mr
    }

    #[tokio::test]
    async fn cascade_submit_returns_results_for_all_bundles() {
        let mr = make_multi_relay(100);
        let bundles = vec![
            BundlePayload {
                bundle_hash: "0x001".into(),
                ..Default::default()
            },
            BundlePayload {
                bundle_hash: "0x002".into(),
                ..Default::default()
            },
        ];
        let results = mr.cascade_submit(bundles).await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn accepted_bundle_tracked_then_reconciled() {
        let mr = make_multi_relay(100);
        let bundle = BundlePayload {
            bundle_hash: "0xacc".into(),
            txs: vec!["0xdeadbeef".into()],
            block_number: "0x64".into(), // block 100
            ..Default::default()
        };
        mr.cascade_submit(vec![bundle]).await;
        assert_eq!(
            mr.pending_confirmations(),
            1,
            "accepted bundle must be pending confirmation"
        );

        // Past target block + grace window, with an unreachable confirmation RPC —
        // must resolve (as not-included) rather than staying pending forever.
        let results = mr
            .reconcile_inclusions(100 + CONFIRMATION_GRACE_BLOCKS)
            .await;
        assert_eq!(results.len(), 1);
        assert_eq!(mr.pending_confirmations(), 0);
    }

    #[tokio::test]
    async fn dedup_prevents_double_submission() {
        let mr = make_multi_relay(50);
        let pos = PositionKey::from_bytes(b"aave:0xabc:usdc");
        assert!(mr.claim_position(&pos, 50).is_ok());
        let result = mr.claim_position(&pos, 51);
        assert!(
            matches!(result, Err(RelayError::DuplicateSubmission { .. })),
            "second claim must fail"
        );
    }

    #[tokio::test]
    async fn blacklist_check_works() {
        let mr = make_multi_relay(0);
        assert!(mr.is_builder_blacklisted("0xbad"));
        assert!(!mr.is_builder_blacklisted("0xgood"));
    }

    #[test]
    fn on_new_block_does_not_panic() {
        let mr = make_multi_relay(0);
        for b in 0u64..100 {
            mr.on_new_block(b, [b as u8; 32]);
        }
    }

    #[test]
    fn sequencer_restart_clears_dedup() {
        let mr = make_multi_relay(100);
        let pos = PositionKey::from_bytes(b"compound:0xdef:weth");
        mr.claim_position(&pos, 100).unwrap();
        mr.on_sequencer_restart(200);
        assert!(mr.claim_position(&pos, 200).is_ok());
    }

    #[test]
    fn ranked_relays_non_empty_after_seeding() {
        let mr = make_multi_relay(0);
        let ranked = mr.ranked_relays();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].relay, RelayName::Flashbots);
    }

    // ── Audit fix regression test (this revision) ─────────────────────────────

    #[tokio::test]
    async fn new_returns_a_live_reorg_event_receiver() {
        let f = make_blacklist_file();
        let blacklist = BuilderBlacklist::load(f.path()).unwrap();
        let metrics = LaRelayMetrics::new(10, ExecutionAddress("0xEVT".into()));
        let mut clients: HashMap<String, Arc<dyn RelayClient>> = HashMap::new();
        clients.insert("flashbots".into(), Arc::new(MockRelayClient::new(true)));
        let cfg = RelayConfig {
            confirmation_rpc_url: "http://localhost:1".into(),
            ..Default::default()
        };

        let (mr, mut event_rx) = MultiRelayClient::new(clients, metrics, blacklist, &cfg, 0);

        // Trigger a real reorg and confirm the event actually arrives through
        // the receiver this constructor now returns, instead of vanishing.
        mr.on_bundle_submitted(TxHash("0xreorgtest".into()), 500);
        mr.on_new_block(500, [1u8; 32]);
        mr.on_new_block(500, [2u8; 32]); // different hash at same height -> reorg

        let event = tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv())
            .await
            .expect("must not time out")
            .expect("channel must not be closed");
        assert_eq!(event.orphaned_block, 500);
    }
}