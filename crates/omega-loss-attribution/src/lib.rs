// crates/omega-loss-attribution/src/lib.rs
//
// omega-loss-attribution — Loss Attribution Engine for the Omega Engine.
//
// ## Architectural role (§22.1)
//
//   omega-loss-attribution ← omega-core
//   omega-gas-war          ← omega-loss-attribution  (not the reverse)
//   omega-strategies       ← omega-loss-attribution
//
//   This crate produces per-FeatureKey fee multipliers that omega-gas-war
//   consumes for adaptive cap computation.  It does NOT depend on
//   omega-gas-war.
//
// ## Module map
//
//   classifier.rs          — LossCode (10-class taxonomy, §13, §13.4),
//                            LossEvent (ML training signal),
//                            FeatureKey (per-group multiplier key)
//
//   online_learner.rs      — GasModelOnlineLearner: online gradient
//                            descent, 80/20 holdout split (fix C1),
//                            ceiling escalation pause (fix I5) (§13.1, §13.3)
//
//   checkpoint.rs          — save/load/prune ModelCheckpoint bincode files
//                            (§13.2, fix I1)
//
//   ceiling_escalation.rs  — CeilingEscalationTracker: state for the
//                            GET /api/v1/la/gas-model/ceiling-status API
//                            (§13.3, §17.2)
//
//   validation.rs          — ExecutionTrace + PipelineLossEvent validators;
//                            AttributionValidator for cross-type consistency
//
//   dashboard.rs           — AttributionDashboard: pipeline observability
//                            aggregator (§16, §17.2)

pub mod ceiling_escalation;
pub mod checkpoint;
pub mod classifier;
pub mod dashboard;
pub mod online_learner;
pub mod validation;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use classifier::{asset_tier, hf_urgency_tier, size_tier, FeatureKey, LossCode, LossEvent};

pub use checkpoint::{
    list_checkpoints, load_latest, load_version, save as save_checkpoint, CheckpointMeta,
    ModelCheckpoint,
};

pub use online_learner::GasModelOnlineLearner;

pub use ceiling_escalation::{CeilingEscalationState, CeilingEscalationTracker};

pub use validation::{
    AttributionValidator, ExecutionTrace, ExecutionTraceValidator, PipelineLossEvent,
    PipelineLossEventValidator, ValidationError, ValidationResult, Validator,
};

pub use dashboard::{AttributionDashboard, DashboardSnapshot};
