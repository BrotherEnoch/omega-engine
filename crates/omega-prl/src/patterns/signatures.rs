// omega-prl/src/signatures.rs
//! Pattern signature definitions — §8.2, §8.3
//!
//! Pattern signatures are:
//!   - Versioned (u32 version field)
//!   - Governance-controlled (§20.1)
//!   - Atomically swappable (hot-reload via ArcSwap)
//!   - Rollback-capable (§8.3, §20.1)

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

// ─────────────────────────────────────────────────────────────────────────────
// Pattern domain — maps to §3.1 responsibility table
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum PatternDomain {
    GasWar = 0,
    RelayBehavior = 1,
    LiquidationTiming = 2,
    OracleMovement = 3,
    BuilderManipulation = 4,
    SearcherFingerprint = 5,
    SequencerStability = 6,
    ProfitabilityDrift = 7,
    SimulationDrift = 8,
    MevCongestion = 9,
    AddressReputation = 10,
    FailureClustering = 11,
}

/// Opaque pattern identifier — 8 bytes for cache efficiency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PatternId(pub u64);

impl PatternId {
    pub const fn from_domain_seq(domain: PatternDomain, seq: u32) -> Self {
        Self(((domain as u64) << 32) | seq as u64)
    }

    pub fn domain(self) -> u8 {
        (self.0 >> 32) as u8
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pattern type — §8.1
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PatternType {
    Deterministic = 0, // rule-based
    Statistical = 1,   // Z-score / EWMA
    Behavioral = 2,    // signature matching
    Sequence = 3,      // Markov transition chains
    Competitive = 4,   // searcher fingerprinting
    Adversarial = 5,   // anomaly detection
}

// ─────────────────────────────────────────────────────────────────────────────
// §8.2 — PatternSignature
// ─────────────────────────────────────────────────────────────────────────────

/// A versioned, governance-controlled pattern signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternSignature {
    pub id: PatternId,
    /// Monotonically increasing version — incremented on every governance update.
    pub version: u32,
    pub domain: PatternDomain,
    pub pattern_type: PatternType,
    /// Up to 8 threshold values for multi-condition matching.
    pub thresholds: SmallVec<[f64; 8]>,
    /// Feature weights corresponding to threshold indices.
    pub weights: SmallVec<[f64; 8]>,
    /// Minimum confidence required before emitting this pattern (§15.3).
    pub confidence_min: f64,
    /// Human-readable description for governance and audit trails.
    pub description: String,
    /// Whether this signature is currently active.
    pub active: bool,
}

impl PatternSignature {
    /// Compute a deterministic score for this signature given a scalar input.
    /// Used for deterministic (rule-based) patterns (§8.1).
    #[inline]
    pub fn score_scalar(&self, value: f64) -> f64 {
        let mut score = 0.0f64;
        for (i, &threshold) in self.thresholds.iter().enumerate() {
            let weight = self.weights.get(i).copied().unwrap_or(1.0);
            if value >= threshold {
                score += weight;
            }
        }
        score
    }

    /// Compute Z-score based score — used for Statistical patterns.
    #[inline]
    pub fn score_zscore(&self, z: f64) -> f64 {
        // Default threshold[0]: trigger z-score
        let z_threshold = self.thresholds.first().copied().unwrap_or(2.0);
        let weight = self.weights.first().copied().unwrap_or(1.0);
        if z.abs() >= z_threshold {
            weight * (z.abs() / z_threshold).min(3.0)
        } else {
            0.0
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in signature registry — pre-defined signatures for all 12 domains
// ─────────────────────────────────────────────────────────────────────────────

/// Returns the canonical set of built-in signatures. These are the governance
/// baseline — all can be overridden via hot-reload (§8.3).
pub fn builtin_signatures() -> Vec<PatternSignature> {
    vec![
        // ── Gas War ──────────────────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::GasWar, 1),
            version: 1,
            domain: PatternDomain::GasWar,
            pattern_type: PatternType::Statistical,
            thresholds: smallvec::smallvec![2.0, 3.0], // z=2 warn, z=3 critical
            weights: smallvec::smallvec![0.5, 1.0],
            confidence_min: 0.80,
            description: "Gas escalation velocity z-score exceeds threshold".into(),
            active: true,
        },
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::GasWar, 2),
            version: 1,
            domain: PatternDomain::GasWar,
            pattern_type: PatternType::Competitive,
            thresholds: smallvec::smallvec![0.7],
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.80,
            description: "Competitor surge probability above 70%".into(),
            active: true,
        },
        // ── Relay Behavior ───────────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::RelayBehavior, 1),
            version: 1,
            domain: PatternDomain::RelayBehavior,
            pattern_type: PatternType::Statistical,
            thresholds: smallvec::smallvec![3.0], // 3σ anomaly (§10.3)
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.95,
            description: "Relay leak suspicion: >3σ latency anomaly".into(),
            active: true,
        },
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::RelayBehavior, 2),
            version: 1,
            domain: PatternDomain::RelayBehavior,
            pattern_type: PatternType::Deterministic,
            thresholds: smallvec::smallvec![0.15], // >15% inclusion drop (§10.3)
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.85,
            description: "Relay inclusion drop >15% below baseline".into(),
            active: true,
        },
        // ── Liquidation Timing ───────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::LiquidationTiming, 1),
            version: 1,
            domain: PatternDomain::LiquidationTiming,
            pattern_type: PatternType::Behavioral,
            thresholds: smallvec::smallvec![0.8],
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.80,
            description: "High HF velocity — imminent liquidation window".into(),
            active: true,
        },
        // ── Oracle Movement ──────────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::OracleMovement, 1),
            version: 1,
            domain: PatternDomain::OracleMovement,
            pattern_type: PatternType::Adversarial,
            thresholds: smallvec::smallvec![0.05, 0.10], // 5% deviation warn, 10% critical
            weights: smallvec::smallvec![0.5, 1.0],
            confidence_min: 0.90,
            description: "Oracle price deviation suggests manipulation".into(),
            active: true,
        },
        // ── Builder Manipulation ─────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::BuilderManipulation, 1),
            version: 1,
            domain: PatternDomain::BuilderManipulation,
            pattern_type: PatternType::Adversarial,
            thresholds: smallvec::smallvec![0.7],
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.95, // §15.3 relay blacklist threshold
            description: "Builder front-run probability exceeds 70%".into(),
            active: true,
        },
        // ── Sequencer Stability ──────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::SequencerStability, 1),
            version: 1,
            domain: PatternDomain::SequencerStability,
            pattern_type: PatternType::Statistical,
            thresholds: smallvec::smallvec![0.85],
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.85, // §15.3 sequencer instability
            description: "Sequencer restart probability >85%".into(),
            active: true,
        },
        // ── Failure Clustering ───────────────────────────────────────────────
        PatternSignature {
            id: PatternId::from_domain_seq(PatternDomain::FailureClustering, 1),
            version: 1,
            domain: PatternDomain::FailureClustering,
            pattern_type: PatternType::Behavioral,
            thresholds: smallvec::smallvec![5.0], // 5 correlated losses in Short window
            weights: smallvec::smallvec![1.0],
            confidence_min: 0.80,
            description: "Correlated loss cluster detected".into(),
            active: true,
        },
    ]
}
