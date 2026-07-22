// omega-prl/src/lib.rs
//! # omega-prl
//!
//! OmegaEngine v12 Pattern Recognition Layer (PRL).
//!
//! ## Design invariants (spec §1, §2.3, §17.2)
//!
//! 1. **Deterministic** — identical input stream → identical pattern outputs.
//! 2. **Non-blocking** — PRL NEVER blocks liquidation execution, relay
//!    submission, gas bidding, oracle ingestion, or FSM transitions.
//! 3. **Advisory-only** — all outputs are hints; execution systems may ignore.
//! 4. **Governance-controlled** — pattern weights and model versions are
//!    versioned, checkpointed, and rollback-capable.
//! 5. **Replayable** — all pattern outputs reproducible from WAL event logs.
//! 6. **Isolated** — PRL failure MUST NOT halt any execution path.
//!
//! ## Parallelism model (§22.3)
//!
//! The PRL runs one Tokio worker task per shard (shard_count == physical cores
//! on the NUMA node).  Each shard owns a ring buffer slice and an isolated
//! `FeatureExtractor`.  Pattern scoring uses a shared `Arc<PatternMatcher>`
//! whose `DashMap` internals provide shard-level locking — no global mutex
//! on the hot path.  The Rayon thread pool is used for parallel replay.
//!
//! ## Audit note (this revision)
//!
//! `drain_shard_tick` previously kept scoring events even while
//! `PrlHealth` was `Halted` — only the `get_*` read accessors checked
//! `is_halted()`. Fixed below: the ingestion path now discards (rather
//! than scores) events while halted. See that method's comment.
//!
//! `PrlEngine` (below) is flagged `#[deprecated]` — it references
//! `crate::pattern::PatternMatcher` (singular module) and
//! `omega_core::types::signal::Signal`, neither of which match the
//! production pipeline (`PatternRecognitionLayer`,
//! `crate::patterns::matcher::PatternMatcher`) or this crate's actual
//! `omega_core` dependency (which exports `OracleSignal`, not `Signal`).
//! It calls `self.matcher.match_pattern(signal)`, a method that does not
//! exist on the real `PatternMatcher`. This looks like orphaned scaffold
//! code that bypasses `PrlHealth`, the WAL, confidence decay, and
//! threshold governance entirely via a separate, unaudited
//! `PrlSafetyGate`. See the warning on the struct itself.

#![deny(unsafe_op_in_unsafe_fn)]

// ─────────────────────────────────────────────────────────────────────────────
// Module declarations — mirrors §23 module structure exactly
// ─────────────────────────────────────────────────────────────────────────────

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

pub mod pattern;
pub mod inference;
pub mod safety;

use omega_core::types::signal::Signal;
use tracing;

/// AUDIT WARNING (see module doc comment above): this struct uses
/// `crate::pattern::PatternMatcher` (singular module) and
/// `omega_core::types::signal::Signal` — NEITHER of which match the
/// production pipeline (`crate::patterns::matcher::PatternMatcher`,
/// `PatternRecognitionLayer`) or the `omega_core::types::signal::OracleSignal`
/// type this crate's own dependency actually exports.
/// `process_signal` calls `self.matcher.match_pattern(signal)`, a method
/// that does not exist on the real `PatternMatcher` reviewed in this
/// audit. This looks like orphaned scaffold code that either fails to
/// compile against the rest of this crate, or — if `pattern.rs` /
/// `inference.rs` / `safety.rs` define a fully separate type universe —
/// completely bypasses `PrlHealth`, the WAL, confidence decay, and
/// threshold governance via a separate, unaudited `PrlSafetyGate`.
/// DO NOT treat this as a safe alternative entry point until it is
/// reconciled with — or removed in favor of — `PatternRecognitionLayer`.
#[deprecated(
    note = "unreconciled with PatternRecognitionLayer's health/WAL/confidence \
            pipeline — see the audit warning on this struct's doc comment"
)]
pub struct PrlEngine {
    matcher: pattern::PatternMatcher,
    safety: safety::PrlSafetyGate,
}

#[allow(deprecated)]
impl PrlEngine {
    pub fn new() -> Self {
        Self {
            matcher: pattern::PatternMatcher::new(),
            safety: safety::PrlSafetyGate::new(),
        }
    }

    pub fn process_signal(&self, signal: Signal) -> Option<Signal> {
        if !self.safety.is_safe_to_process() {
            tracing::warn!("PRL blocked by safety gate (kill switch or budget exceeded)");
            return None;
        }

        self.matcher.match_pattern(signal)
    }
}

pub mod integration;

// ─────────────────────────────────────────────────────────────────────────────
// Re-exports — §25.1 internal API surface
// ─────────────────────────────────────────────────────────────────────────────

pub use ingestion::event_bus::{EventBus, EventPriority, EventSource, EventType, PatternEvent};
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
pub use ml::inference::{MODEL_GAS_WAR, MODEL_LIQUIDATION, MODEL_RELAY, MODEL_SEARCHER};

pub use governance::rollback::SignatureRollbackManager;
pub use governance::signatures::{GovernanceAction, GovernanceActionType, GovernanceAuditLog};
pub use governance::thresholds::ThresholdConfig;

pub use replay::deterministic::ReplayArchive;
pub use replay::verifier::{DivergenceKind, ReplayDivergence, ReplayVerifier};

pub use health::degraded::{PrlHealth, PrlHealthState};
pub use health::watchdog::PrlWatchdog;

pub use metrics::events::ObservabilityEvent;
pub use metrics::prometheus::PrlMetrics;

// ─────────────────────────────────────────────────────────────────────────────
// Top-level PRL handle
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use tracing::{info, warn};

/// Configuration for the PRL.
#[derive(Debug, Clone)]
pub struct PrlConfig {
    pub model_dir: std::path::PathBuf,
    pub wal_dir: std::path::PathBuf,
    pub max_inference_latency_us: u64,
    pub ring_buffer_capacity: usize,
    pub shard_count: usize,
    /// Phase 0: shadow mode — no execution influence.
    pub shadow_mode: bool,
    pub thresholds: ThresholdConfig,
}

impl Default for PrlConfig {
    fn default() -> Self {
        Self {
            model_dir: std::path::PathBuf::from("/var/omega/prl-models"),
            wal_dir: std::path::PathBuf::from("/var/omega/prl"),
            max_inference_latency_us: 50,
            ring_buffer_capacity: 1 << 20,
            shard_count: 8,
            shadow_mode: true,
            thresholds: ThresholdConfig::default(),
        }
    }
}

/// Top-level Pattern Recognition Layer handle.
///
/// All public methods are non-blocking.  Internal processing runs on
/// dedicated per-shard Tokio worker tasks.
pub struct PatternRecognitionLayer {
    pub config: PrlConfig,
    pub health: Arc<PrlHealth>,
    pub metrics: Arc<PrlMetrics>,
    pub bus: Arc<EventBus>,
    pub matcher: Arc<PatternMatcher>,
    pub ml: Arc<OnnxInferenceEngine>,
    pub replay: Arc<ReplayArchive>,
    pub watchdog: PrlWatchdog,
    pub thresholds: Arc<ArcSwap<ThresholdConfig>>,
}

impl PatternRecognitionLayer {
    /// Construct and start the PRL.
    pub async fn new(config: PrlConfig) -> anyhow::Result<Self> {
        info!(
            shadow_mode = config.shadow_mode,
            shards = config.shard_count,
            "Initialising PRL v1.0"
        );

        let health = Arc::new(PrlHealth::new());
        let metrics = Arc::new(PrlMetrics::new()?);
        let thresholds = Arc::new(ArcSwap::from_pointee(config.thresholds.clone()));
        let replay = Arc::new(ReplayArchive::open(&config.wal_dir).await?);

        // EventBus::new(shard_count, ring_buffer_capacity) — matches updated signature.
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
                warn!(error = %e, "PRL ML engine failed to load — heuristic fallback active");
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

    // ── §25.1 Internal API ────────────────────────────────────────────────────

    #[inline]
    pub fn get_pattern_score(&self, key: &PatternId) -> Option<ConfidenceScore> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.get_score(key)
    }

    #[inline]
    pub fn get_relay_risk(&self, relay_id: u32) -> Option<RelayScore> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.relay_score(relay_id)
    }

    #[inline]
    pub fn get_searcher_fingerprint(&self, id: u64) -> Option<SearcherFingerprint> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.searcher_fingerprint(id)
    }

    #[inline]
    pub fn get_gas_forecast(&self) -> Option<GasWarForecast> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.gas_forecast()
    }

    #[inline]
    pub fn get_liquidation_risk(&self, position_key: u64) -> Option<LiquidationPattern> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.liquidation_risk(position_key)
    }

    #[inline]
    pub fn get_sequencer_risk(&self) -> Option<SequencerRiskScore> {
        if self.health.is_halted() {
            return None;
        }
        self.matcher.sequencer_risk()
    }

    /// Ingest an event.  Non-blocking — drops low-priority events under
    /// backpressure rather than blocking the caller (§5.2).
    ///
    /// `publish` takes ownership; WAL append and shard routing happen inside.
    #[inline]
    pub fn ingest(&self, event: PatternEvent) {
        self.metrics.events_ingested.inc();
        self.replay.append(&event);
        // EventBus::publish takes PatternEvent by value — correct call.
        self.bus.publish(event);
    }

    pub async fn replay_window(
        &self,
        from_ts: u64,
        to_ts: u64,
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
                let prl = Arc::clone(self);
                let mut shut = shutdown.clone();
                tokio::spawn(async move {
                    prl.run_shard(shard_idx, &mut shut).await;
                })
            })
            .collect()
    }

    async fn run_shard(&self, shard_idx: usize, shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        let mut extractor = FeatureExtractor::new();
        let mut ticker = tokio::time::interval(Duration::from_micros(500));
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
        // drain_shard is now defined on EventBus — correct call.
        self.bus.drain_shard(shard_idx, &mut buf, 256);

        // Previously ungated: pattern matching kept running even while
        // PrlHealth was Halted — only the read accessors (get_pattern_score,
        // etc.) checked is_halted(). Draining still happens (so the shard's
        // queue doesn't grow unboundedly while halted) but events are
        // discarded rather than scored, matching the crate's own stated
        // invariant that a HALTED PRL disables all advisory outputs.
        if self.health.is_halted() {
            self.metrics.set_queue_depth(0);
            return;
        }

        for event in buf {
            if let Some(fv) = extractor.extract(&event) {
                self.matcher.process(&fv);
            }
        }

        self.metrics.set_queue_depth(0);
    }
}