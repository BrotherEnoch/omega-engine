// omega-prl/src/lib.rs
//! # omega-prl
//!
//! OmegaEngine v12 Pattern Recognition Layer (PRL).
//!
//! ## Design invariants (spec Â§1, Â§2.3, Â§17.2)
//!
//! 1. **Deterministic** â€” identical input stream â†’ identical pattern outputs.
//! 2. **Non-blocking** â€” PRL NEVER blocks liquidation execution, relay
//!    submission, gas bidding, oracle ingestion, or FSM transitions.
//! 3. **Advisory-only** â€” all outputs are hints; execution systems may ignore.
//! 4. **Governance-controlled** â€” pattern weights and model versions are
//!    versioned, checkpointed, and rollback-capable.
//! 5. **Replayable** â€” all pattern outputs reproducible from WAL event logs.
//! 6. **Isolated** â€” PRL failure MUST NOT halt any execution path.
//!
//! ## Parallelism model (Â§22.3)
//!
//! The PRL runs one Tokio worker task per shard (shard_count == physical cores
//! on the NUMA node).  Each shard owns a ring buffer slice and an isolated
//! `FeatureExtractor`.  Pattern scoring uses a shared `Arc<PatternMatcher>`
//! whose `DashMap` internals provide shard-level locking â€” no global mutex
//! on the hot path.  The Rayon thread pool is used for parallel replay.

#![deny(unsafe_op_in_unsafe_fn)]

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Module declarations â€” mirrors Â§23 module structure exactly
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub mod ingestion {
    pub mod event_bus;
    pub mod ring_buffer;
    pub mod wal;
}

pub mod features {
    pub mod extractor;
    pub mod simd;
    pub mod temporal;
}

pub mod patterns {
    pub mod gas_war;
    pub mod liquidation;
    pub mod matcher;
    pub mod relay;
    pub mod searcher;
    pub mod sequencer;
    pub mod signatures;
}

pub mod scoring {
    pub mod confidence;
    pub mod decay;
    pub mod ranking;
}

pub mod ml {
    pub mod checkpoints;
    pub mod fallback;
    pub mod inference;
    pub mod validation;
}

pub mod governance {
    pub mod rollback;
    pub mod signatures;
    pub mod thresholds;
}

pub mod replay {
    pub mod deterministic;
    pub mod verifier;
}

pub mod health {
    pub mod degraded;
    pub mod watchdog;
}

pub mod metrics {
    pub mod events;
    pub mod prometheus;
}

pub mod integration;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Re-exports â€” Â§25.1 internal API surface
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use ingestion::event_bus::{
    EventBus, EventPriority, EventSource, EventType, PatternEvent,
};
pub use ingestion::ring_buffer::LockFreeRingBuffer;
pub use ingestion::wal::EventWal;

pub use features::extractor::FeatureExtractor;
pub use features::temporal::{TemporalAggregator, TemporalBucket, WindowClass};

pub use patterns::gas_war::GasWarForecast;
pub use patterns::liquidation::LiquidationPattern;
pub use patterns::matcher::PatternMatcher;
pub use patterns::relay::RelayScore;
pub use patterns::searcher::SearcherFingerprint;
pub use patterns::sequencer::SequencerRiskScore;
pub use patterns::signatures::{builtin_signatures, PatternDomain, PatternId, PatternSignature};

pub use scoring::confidence::ConfidenceScore;
pub use scoring::decay::decay_confidence;
pub use scoring::ranking::MinConfidenceThresholds;

pub use ml::checkpoints::ModelCheckpointStore;
pub use ml::fallback::DeterministicFallback;
pub use ml::inference::{InferenceResult, OnnxInferenceEngine};
pub use ml::inference::{
    MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_RELAY, MODEL_SEARCHER,
};

pub use governance::rollback::SignatureRollbackManager;
pub use governance::signatures::{GovernanceAction, GovernanceActionType, GovernanceAuditLog};
pub use governance::thresholds::ThresholdConfig;

pub use replay::deterministic::ReplayArchive;
pub use replay::verifier::{DivergenceKind, ReplayDivergence, ReplayVerifier};

pub use health::degraded::{PrlHealth, PrlHealthState};
pub use health::watchdog::PrlWatchdog;

pub use metrics::events::ObservabilityEvent;
pub use metrics::prometheus::PrlMetrics;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Top-level PRL handle
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tracing::{info, warn};

/// Configuration for the PRL.
#[derive(Debug, Clone)]
pub struct PrlConfig {
    pub model_dir:                std::path::PathBuf,
    pub wal_dir:                  std::path::PathBuf,
    pub max_inference_latency_us: u64,
    pub ring_buffer_capacity:     usize,
    pub shard_count:              usize,
    /// Phase 0: shadow mode â€” no execution influence.
    pub shadow_mode:              bool,
    pub thresholds:               ThresholdConfig,
}

impl Default for PrlConfig {
    fn default() -> Self {
        Self {
            model_dir:                std::path::PathBuf::from("/var/omega/prl-models"),
            wal_dir:                  std::path::PathBuf::from("/var/omega/prl"),
            max_inference_latency_us: 50,
            ring_buffer_capacity:     1 << 20,
            shard_count:              8,
            shadow_mode:              true,
            thresholds:               ThresholdConfig::default(),
        }
    }
}

/// Top-level Pattern Recognition Layer handle.
///
/// All public methods are non-blocking.  Internal processing runs on
/// dedicated per-shard Tokio worker tasks.
pub struct PatternRecognitionLayer {
    pub config:     PrlConfig,
    pub health:     Arc<PrlHealth>,
    pub metrics:    Arc<PrlMetrics>,
    pub bus:        Arc<EventBus>,
    pub matcher:    Arc<PatternMatcher>,
    pub ml:         Arc<OnnxInferenceEngine>,
    pub replay:     Arc<ReplayArchive>,
    pub watchdog:   PrlWatchdog,
    pub thresholds: Arc<ArcSwap<ThresholdConfig>>,
}

impl PatternRecognitionLayer {
    /// Construct and start the PRL.
    pub async fn new(config: PrlConfig) -> anyhow::Result<Self> {
        info!(
            shadow_mode = config.shadow_mode,
            shards      = config.shard_count,
            "Initialising PRL v1.0"
        );

        let health     = Arc::new(PrlHealth::new());
        let metrics    = Arc::new(PrlMetrics::new()?);
        let thresholds = Arc::new(ArcSwap::from_pointee(config.thresholds.clone()));
        let replay     = Arc::new(ReplayArchive::open(&config.wal_dir).await?);

        // EventBus::new(shard_count, ring_buffer_capacity) â€” matches updated signature.
        let bus = Arc::new(EventBus::new(
            config.shard_count,
            config.ring_buffer_capacity,
        ));
        let matcher = Arc::new(PatternMatcher::new(Arc::clone(&thresholds)));

        let ml = match OnnxInferenceEngine::load(&config.model_dir) {
            Ok(engine) => {
                info!("PRL ML engine loaded successfully");
                Arc::new(engine)
            }
            Err(e) => {
                warn!(error = %e, "PRL ML engine failed to load â€” heuristic fallback active");
                health.set_degraded("ML_LOAD_FAILURE");
                Arc::new(OnnxInferenceEngine::heuristic_fallback())
            }
        };

        let watchdog = PrlWatchdog::new(
            Arc::clone(&health),
            Arc::clone(&metrics),
            config.max_inference_latency_us,
        );

        Ok(Self {
            config,
            health,
            metrics,
            bus,
            matcher,
            ml,
            replay,
            watchdog,
            thresholds,
        })
    }

    // â”€â”€ Â§25.1 Internal API â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    #[inline]
    pub fn get_pattern_score(&self, key: &PatternId) -> Option<ConfidenceScore> {
        if self.health.is_halted() { return None; }
        self.matcher.get_score(key)
    }

    #[inline]
    pub fn get_relay_risk(&self, relay_id: u32) -> Option<RelayScore> {
        if self.health.is_halted() { return None; }
        self.matcher.relay_score(relay_id)
    }

    #[inline]
    pub fn get_searcher_fingerprint(&self, id: u64) -> Option<SearcherFingerprint> {
        if self.health.is_halted() { return None; }
        self.matcher.searcher_fingerprint(id)
    }

    #[inline]
    pub fn get_gas_forecast(&self) -> Option<GasWarForecast> {
        if self.health.is_halted() { return None; }
        self.matcher.gas_forecast()
    }

    #[inline]
    pub fn get_liquidation_risk(&self, position_key: u64) -> Option<LiquidationPattern> {
        if self.health.is_halted() { return None; }
        self.matcher.liquidation_risk(position_key)
    }

    #[inline]
    pub fn get_sequencer_risk(&self) -> Option<SequencerRiskScore> {
        if self.health.is_halted() { return None; }
        self.matcher.sequencer_risk()
    }

    /// Ingest an event.  Non-blocking â€” drops low-priority events under
    /// backpressure rather than blocking the caller (Â§5.2).
    ///
    /// `publish` takes ownership; WAL append and shard routing happen inside.
    #[inline]
    pub fn ingest(&self, event: PatternEvent) {
        self.metrics.events_ingested.inc();
        self.replay.append(&event);
        // EventBus::publish takes PatternEvent by value â€” correct call.
        self.bus.publish(event);
    }

    pub async fn replay_window(
        &self,
        from_ts: u64,
        to_ts:   u64,
    ) -> anyhow::Result<Vec<ConfidenceScore>> {
        self.replay.replay(from_ts, to_ts).await
    }

    pub fn reload_thresholds(&self, new: ThresholdConfig) {
        self.thresholds.store(Arc::new(new));
        info!("PRL thresholds hot-reloaded");
    }

    #[inline]
    pub fn health_state(&self) -> PrlHealthState {
        self.health.state()
    }

    /// Start per-shard worker tasks.
    pub fn start_shard_workers(
        self: &Arc<Self>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        let shard_count = self.config.shard_count;
        (0..shard_count)
            .map(|shard_idx| {
                let prl      = Arc::clone(self);
                let mut shut = shutdown.clone();
                tokio::spawn(async move {
                    prl.run_shard(shard_idx, &mut shut).await;
                })
            })
            .collect()
    }

    async fn run_shard(
        &self,
        shard_idx: usize,
        shutdown:  &mut tokio::sync::watch::Receiver<bool>,
    ) {
        let mut extractor = FeatureExtractor::new();
        let mut ticker    = tokio::time::interval(Duration::from_micros(500));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.drain_shard_tick(shard_idx, &mut extractor);
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }
    }

    #[inline]
    fn drain_shard_tick(&self, shard_idx: usize, extractor: &mut FeatureExtractor) {
        let mut buf = Vec::with_capacity(256);
        // drain_shard is now defined on EventBus â€” correct call.
        self.bus.drain_shard(shard_idx, &mut buf, 256);

        for event in buf {
            if let Some(fv) = extractor.extract(&event) {
                self.matcher.process(&fv);
            }
        }

        self.metrics.set_queue_depth(0);
    }
}