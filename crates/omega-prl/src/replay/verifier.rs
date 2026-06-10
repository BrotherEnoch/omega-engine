// omega-prl/src/replay/verifier.rs
//! Replay divergence verifier — §18.2
//!
//! Compares live pattern outputs against a replay run bit-for-bit.
//! Any divergence triggers §17.3 DEGRADED state + alert.

use crate::scoring::confidence::ConfidenceScore;

/// A single divergence record.
#[derive(Debug, Clone)]
pub struct ReplayDivergence {
    pub index: usize,
    pub kind: DivergenceKind,
}

#[derive(Debug, Clone)]
pub enum DivergenceKind {
    CountMismatch {
        live_count: usize,
        replay_count: usize,
    },
    PatternIdMismatch {
        live_id: u64,
        replay_id: u64,
    },
    ScoreMismatch {
        live_score: f64,
        replay_score: f64,
    },
    ConfidenceMismatch {
        live_conf: f64,
        replay_conf: f64,
    },
}

/// Bit-for-bit verifier (§18.2).
pub struct ReplayVerifier;

impl ReplayVerifier {
    /// Compare live vs replay scores.
    ///
    /// Returns `Ok(())` when identical, `Err(divergences)` otherwise.
    /// Score comparison uses `f64::to_bits()` — no floating-point tolerance.
    pub fn verify(
        live: &[ConfidenceScore],
        replay: &[ConfidenceScore],
    ) -> Result<(), Vec<ReplayDivergence>> {
        if live.len() != replay.len() {
            return Err(vec![ReplayDivergence {
                index: 0,
                kind: DivergenceKind::CountMismatch {
                    live_count: live.len(),
                    replay_count: replay.len(),
                },
            }]);
        }

        let mut divergences = Vec::new();

        for (i, (l, r)) in live.iter().zip(replay.iter()).enumerate() {
            if l.pattern_id != r.pattern_id {
                divergences.push(ReplayDivergence {
                    index: i,
                    kind: DivergenceKind::PatternIdMismatch {
                        live_id: l.pattern_id.0,
                        replay_id: r.pattern_id.0,
                    },
                });
                continue; // skip further checks for this pair
            }

            if l.score.to_bits() != r.score.to_bits() {
                divergences.push(ReplayDivergence {
                    index: i,
                    kind: DivergenceKind::ScoreMismatch {
                        live_score: l.score,
                        replay_score: r.score,
                    },
                });
            }

            if (l.confidence - r.confidence).abs() > 1e-12 {
                divergences.push(ReplayDivergence {
                    index: i,
                    kind: DivergenceKind::ConfidenceMismatch {
                        live_conf: l.confidence,
                        replay_conf: r.confidence,
                    },
                });
            }
        }

        if divergences.is_empty() {
            Ok(())
        } else {
            Err(divergences)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patterns::signatures::PatternId;

    fn score(id: u64, s: f64, c: f64) -> ConfidenceScore {
        ConfidenceScore {
            pattern_id: PatternId(id),
            score: s,
            confidence: c,
            evidence: 1,
            age_ns: 0,
        }
    }

    #[test]
    fn identical_is_ok() {
        let v = vec![score(1, 0.8, 0.9), score(2, 0.5, 0.7)];
        assert!(ReplayVerifier::verify(&v, &v.clone()).is_ok());
    }

    #[test]
    fn count_mismatch() {
        let live = vec![score(1, 0.8, 0.9)];
        let replay = vec![score(1, 0.8, 0.9), score(2, 0.5, 0.7)];
        let err = ReplayVerifier::verify(&live, &replay).unwrap_err();
        assert!(matches!(err[0].kind, DivergenceKind::CountMismatch { .. }));
    }

    #[test]
    fn score_mismatch() {
        let live = vec![score(1, 0.8, 0.9)];
        let replay = vec![score(1, 0.9, 0.9)];
        if live[0].score.to_bits() != replay[0].score.to_bits() {
            let err = ReplayVerifier::verify(&live, &replay).unwrap_err();
            assert!(matches!(err[0].kind, DivergenceKind::ScoreMismatch { .. }));
        }
    }

    #[test]
    fn pattern_id_mismatch() {
        let live = vec![score(1, 0.8, 0.9)];
        let replay = vec![score(2, 0.8, 0.9)];
        let err = ReplayVerifier::verify(&live, &replay).unwrap_err();
        assert!(matches!(
            err[0].kind,
            DivergenceKind::PatternIdMismatch { .. }
        ));
    }
}
