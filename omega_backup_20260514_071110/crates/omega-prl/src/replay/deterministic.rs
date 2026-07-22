// omega-prl/src/replay/deterministic.rs
//! Deterministic WAL-backed replay â€” Â§18
//!
//! Replay MUST reproduce pattern outputs bit-for-bit (Â§18.2).
//! Same input stream â†’ same outputs because:
//!   1. Feature extraction is pure and deterministic (Â§6.1).
//!   2. Pattern scoring uses only the feature vector and signature weights.
//!   3. Confidence decay uses only the event timestamp from the event itself.
//!
//! Parallelism: events are replayed through per-shard isolated matchers running
//! concurrently on a Rayon thread pool, one shard per domain.  Each shard
//! produces deterministic scores; the final merge is sorted by (pattern_id, ts).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use tracing::{error, info};

use crate::governance::thresholds::ThresholdConfig;
use crate::ingestion::event_bus::PatternEvent;
use crate::ingestion::wal::EventWal;
use crate::features::extractor::FeatureExtractor;
use crate::patterns::matcher::PatternMatcher;
use crate::patterns::signatures::{PatternDomain, builtin_signatures};
use crate::scoring::confidence::ConfidenceScore;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// ReplayArchive
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// WAL-backed deterministic replay archive (Â§18).
pub struct ReplayArchive {
    wal:      EventWal,
    #[allow(dead_code)]
    base_dir: PathBuf,
}

impl ReplayArchive {
    /// Open WAL at `base_dir`.  Creates directories if needed.
    pub async fn open(base_dir: &Path) -> anyhow::Result<Self> {
        let wal = EventWal::open(base_dir).await?;
        Ok(Self { wal, base_dir: base_dir.to_path_buf() })
    }

    /// Append a live event to the WAL.  Called on every ingested event (Â§18.2).
    ///
    /// WAL write failures are logged but NEVER propagated (Â§17.2).
    #[inline]
    pub fn append(&self, event: &PatternEvent) {
        if let Err(e) = self.wal.append(event) {
            error!(error = %e, "PRL WAL write failed â€” replay gap possible");
        }
    }

    /// Replay events in [from_ts, to_ts] and return scored outputs.
    ///
    /// Processing is parallelised across `PatternDomain` slices using Rayon.
    /// Each domain shard runs its own isolated `PatternMatcher` so there are
    /// no shared mutable locks during replay â€” maximises throughput.
    pub async fn replay(
        &self,
        from_ts: u64,
        to_ts:   u64,
    ) -> anyhow::Result<Vec<ConfidenceScore>> {
        info!(from_ts, to_ts, "PRL replay window started");

        let events = self.wal.read_window(from_ts, to_ts)?;
        let n      = events.len();
        info!(event_count = n, "PRL replay: events loaded");

        if events.is_empty() {
            return Ok(Vec::new());
        }

        // Partition events across domains for parallel processing.
        // Each shard gets all events â€” it filters by relevance internally.
        let domains = [
            PatternDomain::GasWar,
            PatternDomain::RelayBehavior,
            PatternDomain::LiquidationTiming,
            PatternDomain::OracleMovement,
            PatternDomain::SequencerStability,
            PatternDomain::SearcherFingerprint,
            PatternDomain::FailureClustering,
            PatternDomain::BuilderManipulation,
            PatternDomain::ProfitabilityDrift,
            PatternDomain::SimulationDrift,
            PatternDomain::MevCongestion,
            PatternDomain::AddressReputation,
        ];

        let thresholds = Arc::new(
            arc_swap::ArcSwap::from_pointee(ThresholdConfig::default())
        );

        // Parallel replay across domains using Rayon.
        let domain_scores: Vec<Vec<ConfidenceScore>> = domains
            .par_iter()
            .map(|&domain| {
                replay_domain(domain, &events, Arc::clone(&thresholds))
            })
            .collect();

        // Merge and de-duplicate (highest confidence per pattern_id).
        let mut merged: std::collections::HashMap<u64, ConfidenceScore> =
            std::collections::HashMap::new();

        for scores in domain_scores {
            for score in scores {
                merged
                    .entry(score.pattern_id.0)
                    .and_modify(|existing| {
                        if score.confidence > existing.confidence {
                            *existing = score;
                        }
                    })
                    .or_insert(score);
            }
        }

        let mut result: Vec<ConfidenceScore> = merged.into_values().collect();
        result.sort_by(|a, b| a.pattern_id.0.cmp(&b.pattern_id.0));

        info!(
            event_count = n,
            score_count = result.len(),
            "PRL replay window complete"
        );

        Ok(result)
    }

    pub fn wal(&self) -> &EventWal { &self.wal }
}

/// Replay one domain shard â€” isolated, no shared state.
fn replay_domain(
    domain:     PatternDomain,
    events:     &[PatternEvent],
    thresholds: Arc<arc_swap::ArcSwap<ThresholdConfig>>,
) -> Vec<ConfidenceScore> {
    // Filter signatures to this domain only.
    let domain_sigs: Vec<_> = builtin_signatures()
        .into_iter()
        .filter(|s| s.domain == domain)
        .collect();

    if domain_sigs.is_empty() {
        return Vec::new();
    }

    let matcher   = PatternMatcher::with_signatures(domain_sigs, thresholds);
    let extractor = FeatureExtractor::new();

    for event in events {
        if let Some(fv) = extractor.extract(event) {
            matcher.process(&fv);
        }
    }

    matcher.drain_scores()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_event_list_returns_empty() {
        let thresholds = Arc::new(arc_swap::ArcSwap::from_pointee(
            ThresholdConfig::default()
        ));
        let scores = replay_domain(
            PatternDomain::GasWar,
            &[],
            thresholds,
        );
        assert!(scores.is_empty());
    }
}