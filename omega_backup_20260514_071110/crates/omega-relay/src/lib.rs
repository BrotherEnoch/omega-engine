ï»¿// crates/omega-relay/src/lib.rs
//! `omega-relay` â€” Relay submission layer for OmegaEngine v12.
//!
//! ## Responsibilities (per spec)
//! | Spec section | Module |
//! |---|---|
//! | Â§11.2 Cascade backpressure + stagger + anti-fingerprint | [`backpressure`] |
//! | Â§11.3 Sequencer double-spend guard | [`dedup`] |
//! | Â§11.4 Reorg guard during 80 ms LA window | [`reorg`] |
//! | Â§12.3 MEV-Boost builder blacklist | [`blacklist`] |
//! | Â§14.1 Address rotation with time-decay carryover | [`reputation`] |
//! | Â§14.2 Anti-fingerprint round-robin | [`reputation`] |
//! | Â§17.1 WebSocket rate limits | [`config`] |
//! | Per-relay LA inclusion rate metrics | [`metrics`] |
//! | Relay HTTP client | [`client`] |
//!
//! ## Dependency position (inter-crate graph Â§22.1)
//! ```text
//! omega-relay â† omega-core, omega-health
//! ```
//! `omega-relay` has no dependency on strategy, gas-war, or loss-attribution
//! crates â€” it is consumed by those crates, not the other way around.

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
pub mod dedup;
pub mod error;
pub mod metrics;
pub mod reorg;
pub mod reputation;

// â”€â”€ Re-exports for ergonomic import by consumers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use backpressure::{CascadeResult, CascadeSubmitter, RelayRateLimiters};
pub use blacklist::BuilderBlacklist;
pub use client::{BundlePayload, RelayClient, SubmissionOutcome};
pub use config::{RelayConfig, RelayName, WS_RATE_ANONYMOUS, WS_RATE_AUTHENTICATED};
pub use dedup::{PositionKey, SequencerRestartHandler, RESTART_WINDOW_BLOCKS};
pub use error::{RelayError, RelayResult};
pub use metrics::{ExecutionAddress, LaRelayMetrics, RelayRateSnapshot};
pub use reorg::{BlueprintState, LaReorgGuard, LaReorgRiskEvent, TxHash, STABILITY_WINDOW_BLOCKS};
pub use reputation::{carryover_pct, rotate_address, shuffled_submission_order, submission_order};

// â”€â”€ MultiRelayClient â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

use std::collections::HashMap;
use std::sync::Arc;

/// Top-level handle that owns all relay clients, metrics, dedup guard,
/// reorg guard, and builder blacklist.  Constructed once at startup and
/// shared across the engine via `Arc`.
///
/// Consumers call `cascade_submit` or `submit_single` â€” all backpressure,
/// dedup, and blacklist logic is applied transparently.
pub struct MultiRelayClient {
    submitter: CascadeSubmitter,
    dedup: Arc<SequencerRestartHandler>,
    reorg_guard: Arc<LaReorgGuard>,
    blacklist: Arc<BuilderBlacklist>,
    metrics: Arc<LaRelayMetrics>,
    relay_clients: Arc<HashMap<String, Arc<dyn RelayClient>>>,
    rate_limiters: Arc<RelayRateLimiters>,
}

impl MultiRelayClient {
    /// Construct the full relay layer.
    ///
    /// # Arguments
    /// - `relay_clients` â€” map of relay name â†’ `Arc<dyn RelayClient>`
    /// - `metrics` â€” shared `LaRelayMetrics` instance
    /// - `blacklist` â€” loaded `BuilderBlacklist`
    /// - `cfg` â€” relay configuration from `default.toml`
    /// - `startup_block` â€” current chain head block at startup
    pub fn new(
        relay_clients: HashMap<String, Arc<dyn RelayClient>>,
        metrics: Arc<LaRelayMetrics>,
        blacklist: Arc<BuilderBlacklist>,
        cfg: &RelayConfig,
        startup_block: u64,
    ) -> Arc<Self> {
        let relay_clients = Arc::new(relay_clients);
        let submitter = CascadeSubmitter::new(
            Arc::clone(&relay_clients),
            Arc::clone(&metrics),
            cfg,
        );

        let relay_names: Vec<String> = relay_clients.keys().cloned().collect();
        let rate_limiters = Arc::new(RelayRateLimiters::new(
            &relay_names,
            cfg.max_bundles_per_relay_per_second as u32,
        ));

        let (reorg_guard, _event_rx) = LaReorgGuard::new();
        // NOTE: callers should drain `_event_rx` in a background task.

        Arc::new(Self {
            submitter,
            dedup: SequencerRestartHandler::new(startup_block),
            reorg_guard,
            blacklist,
            metrics,
            relay_clients,
            rate_limiters,
        })
    }

    /// Submit a bundle in cascade mode.
    ///
    /// Applies: backpressure stagger, LA-inclusion-rate ordering,
    /// anti-fingerprint shuffle.
    pub async fn cascade_submit(&self, bundles: Vec<BundlePayload>) -> Vec<CascadeResult> {
        self.submitter.submit_cascade(bundles).await
    }

    /// Submit a single bundle (non-cascade LA path).
    ///
    /// Applies: anti-fingerprint round-robin (Â§14.2).
    pub async fn submit_single(&self, bundle: BundlePayload) -> RelayResult<bool> {
        backpressure::submit_single_bundle(
            bundle,
            &self.relay_clients,
            &self.metrics,
            &self.rate_limiters,
        )
        .await
    }

    /// Claim submission rights for a position in the restart window.
    /// Returns `Err(DuplicateSubmission)` if already submitted.
    pub fn claim_position(&self, position: &PositionKey, current_block: u64) -> RelayResult<bool> {
        self.dedup.try_submit(position, current_block)
    }

    /// Notify dedup and reorg guard of a new block header.
    pub fn on_new_block(&self, block: u64, block_hash: [u8; 32]) {
        self.dedup.on_new_block(block);
        self.reorg_guard.on_new_block(block, block_hash);
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

    /// Check if a builder key is blacklisted (Â§12.3).
    pub fn is_builder_blacklisted(&self, key: &str) -> bool {
        self.blacklist.contains(key)
    }

    /// Current LA inclusion-rate ranked relay list.
    pub fn ranked_relays(&self) -> Vec<RelayRateSnapshot> {
        self.metrics.la_ranked_relays()
    }

    /// Access shared metrics for address rotation (Â§14.1).
    pub fn metrics(&self) -> &Arc<LaRelayMetrics> {
        &self.metrics
    }

    /// Access the reorg guard for blueprint status queries.
    pub fn reorg_guard(&self) -> &Arc<LaReorgGuard> {
        &self.reorg_guard
    }
}

// â”€â”€ Integration tests â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[cfg(test)]
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

    fn make_multi_relay(startup_block: u64) -> Arc<MultiRelayClient> {
        let f = make_blacklist_file();
        let blacklist = BuilderBlacklist::load(f.path()).unwrap();

        let addr = ExecutionAddress("0xINTEGRATION".into());
        let metrics = LaRelayMetrics::new(100, addr.clone());

        // Seed metrics for both relays
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
            ..Default::default()
        };

        // BuilderBlacklist::load() moves the file â€” keep it alive via the returned Arc
        let _ = f; // file lives until here; path was read at load time
        MultiRelayClient::new(clients, metrics, blacklist, &cfg, startup_block)
    }

    #[tokio::test]
    async fn cascade_submit_returns_results_for_all_bundles() {
        let mr = make_multi_relay(100);
        let bundles = vec![
            BundlePayload { bundle_hash: "0x001".into(), ..Default::default() },
            BundlePayload { bundle_hash: "0x002".into(), ..Default::default() },
        ];
        let results = mr.cascade_submit(bundles).await;
        assert_eq!(results.len(), 2);
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

        // After restart, same position should be claimable again
        assert!(mr.claim_position(&pos, 200).is_ok());
    }

    #[test]
    fn ranked_relays_non_empty_after_seeding() {
        let mr = make_multi_relay(0);
        let ranked = mr.ranked_relays();
        assert_eq!(ranked.len(), 2);
        // flashbots (90%) should rank above bloxroute (75%)
        assert_eq!(ranked[0].relay, RelayName::Flashbots);
    }
}