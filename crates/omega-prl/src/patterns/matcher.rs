// omega-prl/src/patterns/matcher.rs
//! Pattern matching and scoring engine — §8, §15
//!
//! Thread-safety: `PatternMatcher` is `Send + Sync`.
//! DashMap provides shard-level locking on inserts; reads via `get()` are lock-free.
//! `ArcSwap` gives hot-reload of signatures without any lock (§8.3).
//! `parking_lot::RwLock` used for gas_forecast / sequencer_risk (rare writes, many reads).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tracing::{debug, warn};

use crate::features::extractor::{FeatureVector, FEATURE_DIM};
use crate::features::simd;
use crate::governance::thresholds::ThresholdConfig;
use crate::patterns::gas_war::GasWarForecast;
use crate::patterns::liquidation::LiquidationPattern;
use crate::patterns::relay::RelayScore;
use crate::patterns::searcher::SearcherFingerprint;
use crate::patterns::sequencer::SequencerRiskScore;
use crate::patterns::signatures::{builtin_signatures, PatternId, PatternSignature};
use crate::scoring::confidence::ConfidenceScore;
use crate::scoring::decay::decay_confidence;

// ─────────────────────────────────────────────────────────────────────────────
// LiveScore
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LiveScore {
    pub pattern_id: PatternId,
    pub score: f64,
    pub confidence: f64,
    pub evidence: u32,
    pub last_update_ns: u64,
}

impl LiveScore {
    #[inline]
    fn decayed_confidence(&self, now_ns: u64) -> f64 {
        let age_ms = now_ns.saturating_sub(self.last_update_ns) / 1_000_000;
        decay_confidence(self.confidence, age_ms)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PatternMatcher
// ─────────────────────────────────────────────────────────────────────────────

/// Central pattern matching engine.
///
/// Designed for concurrent access from multiple shard workers:
///   - `process()` — called from N shard tasks in parallel, lock-free per-entry.
///   - `get_score()` / domain lookups — lock-free reads via DashMap.
///   - `reload_signatures()` — atomic swap via ArcSwap, zero downtime (§8.3).
pub struct PatternMatcher {
    signatures: Arc<ArcSwap<Vec<PatternSignature>>>,
    live_scores: DashMap<PatternId, LiveScore>,
    relay_scores: DashMap<u32, RelayScore>,
    gas_forecast: parking_lot::RwLock<Option<GasWarForecast>>,
    sequencer_risk: parking_lot::RwLock<Option<SequencerRiskScore>>,
    liquidation_risks: DashMap<u64, LiquidationPattern>,
    searcher_fps: DashMap<u64, SearcherFingerprint>,
    #[allow(dead_code)]
    thresholds: Arc<ArcSwap<ThresholdConfig>>,
    now_ns: AtomicU64,
}

impl PatternMatcher {
    /// Construct with the full built-in signature set.
    pub fn new(thresholds: Arc<ArcSwap<ThresholdConfig>>) -> Self {
        Self::with_signatures(builtin_signatures(), thresholds)
    }

    /// Construct with a specific signature subset — used by parallel replay shards.
    pub fn with_signatures(
        sigs: Vec<PatternSignature>,
        thresholds: Arc<ArcSwap<ThresholdConfig>>,
    ) -> Self {
        Self {
            signatures: Arc::new(ArcSwap::from_pointee(sigs)),
            live_scores: DashMap::new(),
            relay_scores: DashMap::new(),
            gas_forecast: parking_lot::RwLock::new(None),
            sequencer_risk: parking_lot::RwLock::new(None),
            liquidation_risks: DashMap::new(),
            searcher_fps: DashMap::new(),
            thresholds,
            now_ns: AtomicU64::new(0),
        }
    }

    // ── Hot path ──────────────────────────────────────────────────────────────

    /// Process a feature vector against all active signatures.
    pub fn process(&self, fv: &FeatureVector) {
        let ts = fv.ts_nanos;
        self.now_ns.store(ts, Ordering::Relaxed);

        let sigs = self.signatures.load();
        for sig in sigs.iter() {
            if !sig.active {
                continue;
            }

            let raw_score = self.score_signature(sig, fv);
            if raw_score <= 0.0 {
                continue;
            }

            let max_weight = sig.weights.iter().copied().fold(0.0f64, f64::max);
            let confidence = if max_weight > 0.0 {
                (raw_score / max_weight).clamp(0.0, 1.0)
            } else {
                0.0
            };

            if confidence < sig.confidence_min {
                continue;
            }

            self.live_scores
                .entry(sig.id)
                .and_modify(|s| {
                    s.score = raw_score;
                    s.confidence = confidence;
                    s.evidence += 1;
                    s.last_update_ns = ts;
                })
                .or_insert(LiveScore {
                    pattern_id: sig.id,
                    score: raw_score,
                    confidence,
                    evidence: 1,
                    last_update_ns: ts,
                });

            debug!(id = ?sig.id, score = raw_score, confidence, "PRL pattern match");
        }
    }

    // ── Scoring dispatch — §8.1 ───────────────────────────────────────────────

    fn score_signature(&self, sig: &PatternSignature, fv: &FeatureVector) -> f64 {
        use crate::patterns::signatures::PatternType::*;
        match sig.pattern_type {
            Deterministic => {
                let mut ws = [0.0f32; FEATURE_DIM];
                for (i, &wt) in sig.weights.iter().take(FEATURE_DIM).enumerate() {
                    ws[i] = wt as f32;
                }
                simd::weighted_score(fv, &ws) as f64
            }
            Statistical => {
                let threshold = sig.thresholds.first().copied().unwrap_or(2.0) as f32;
                let n = simd::count_above_threshold(fv, threshold.abs());
                sig.weights.first().copied().unwrap_or(1.0) * n as f64
            }
            Behavioral | Competitive | Sequence => {
                let mut proto = FeatureVector::zeroed();
                for (i, &w) in sig.weights.iter().take(FEATURE_DIM).enumerate() {
                    proto.set(i, w as f32);
                }
                simd::cosine_similarity(fv, &proto) as f64
            }
            Adversarial => {
                let zero = FeatureVector::zeroed();
                let dist = simd::l2_distance(fv, &zero) as f64;
                let scale = sig.thresholds.first().copied().unwrap_or(1.0);
                (dist / scale).clamp(0.0, 1.0)
            }
        }
    }

    // ── Domain-specific score updates ─────────────────────────────────────────

    pub fn update_relay_score(&self, relay_id: u32, score: RelayScore) {
        self.relay_scores.insert(relay_id, score);
    }

    pub fn update_gas_forecast(&self, forecast: GasWarForecast) {
        *self.gas_forecast.write() = Some(forecast);
    }

    pub fn update_sequencer_risk(&self, risk: SequencerRiskScore) {
        *self.sequencer_risk.write() = Some(risk);
    }

    pub fn update_liquidation_risk(&self, key: u64, pattern: LiquidationPattern) {
        self.liquidation_risks.insert(key, pattern);
    }

    pub fn update_searcher_fingerprint(&self, fp: SearcherFingerprint) {
        self.searcher_fps.insert(fp.fingerprint_id, fp);
    }

    // ── §25.1 API lookups — lock-free reads ───────────────────────────────────

    pub fn get_score(&self, id: &PatternId) -> Option<ConfidenceScore> {
        let now = self.now_ns.load(Ordering::Relaxed);
        self.live_scores.get(id).map(|s| ConfidenceScore {
            pattern_id: s.pattern_id,
            score: s.score,
            confidence: s.decayed_confidence(now),
            evidence: s.evidence,
            age_ns: now.saturating_sub(s.last_update_ns),
        })
    }

    pub fn relay_score(&self, relay_id: u32) -> Option<RelayScore> {
        self.relay_scores.get(&relay_id).map(|s| *s)
    }

    pub fn gas_forecast(&self) -> Option<GasWarForecast> {
        self.gas_forecast.read().clone()
    }

    pub fn sequencer_risk(&self) -> Option<SequencerRiskScore> {
        self.sequencer_risk.read().clone()
    }

    pub fn liquidation_risk(&self, key: u64) -> Option<LiquidationPattern> {
        self.liquidation_risks.get(&key).map(|p| p.clone())
    }

    pub fn searcher_fingerprint(&self, id: u64) -> Option<SearcherFingerprint> {
        self.searcher_fps.get(&id).map(|fp| fp.clone())
    }

    // ── Replay support ────────────────────────────────────────────────────────

    pub fn drain_scores(&self) -> Vec<ConfidenceScore> {
        let now = self.now_ns.load(Ordering::Relaxed);
        self.live_scores
            .iter()
            .map(|entry| {
                let s = entry.value();
                ConfidenceScore {
                    pattern_id: s.pattern_id,
                    score: s.score,
                    confidence: s.decayed_confidence(now),
                    evidence: s.evidence,
                    age_ns: now.saturating_sub(s.last_update_ns),
                }
            })
            .collect()
    }

    // ── Hot-reload §8.3 ───────────────────────────────────────────────────────

    pub fn reload_signatures(&self, new_sigs: Vec<PatternSignature>) {
        self.signatures.store(Arc::new(new_sigs));
        warn!("PRL signatures hot-reloaded");
    }
}
