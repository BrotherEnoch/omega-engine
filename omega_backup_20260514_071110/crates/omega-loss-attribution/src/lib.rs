ï»¿// crates/omega-loss-attribution/src/lib.rs
// crates/omega-loss-attribution/src/lib.rs
//
// omega-loss-attribution â€” Loss Attribution Engine for the Omega Engine.
//
// ## Architectural role (Â§22.1)
//
//   omega-loss-attribution â† omega-core
//   omega-gas-war          â† omega-loss-attribution  (not the reverse)
//   omega-strategies       â† omega-loss-attribution
//
//   This crate produces per-FeatureKey fee multipliers that omega-gas-war
//   consumes for adaptive cap computation.  It does NOT depend on
//   omega-gas-war.
//
// ## Module map
//
//   classifier.rs          â€” LossCode (10-class taxonomy, Â§13, Â§13.4),
//                            LossEvent (ML training signal),
//                            FeatureKey (per-group multiplier key)
//
//   online_learner.rs      â€” GasModelOnlineLearner: online gradient
//                            descent, 80/20 holdout split (fix C1),
//                            ceiling escalation pause (fix I5) (Â§13.1, Â§13.3)
//
//   checkpoint.rs          â€” save/load/prune ModelCheckpoint bincode files
//                            (Â§13.2, fix I1)
//
//   ceiling_escalation.rs  â€” CeilingEscalationTracker: state for the
//                            GET /api/v1/la/gas-model/ceiling-status API
//                            (Â§13.3, Â§17.2)
//
//   validation.rs          â€” ExecutionTrace + PipelineLossEvent validators;
//                            AttributionValidator for cross-type consistency
//
//   dashboard.rs           â€” AttributionDashboard: pipeline observability
//                            aggregator (Â§16, Â§17.2)

pub mod ceiling_escalation;
pub mod checkpoint;
pub mod classifier;
pub mod dashboard;
pub mod online_learner;
pub mod validation;

// â”€â”€ Convenience re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use classifier::{
    asset_tier,
    hf_urgency_tier,
    size_tier,
    FeatureKey,
    LossCode,
    LossEvent,
};

pub use checkpoint::{
    list_checkpoints,
    load_latest,
    load_version,
    save as save_checkpoint,
    CheckpointMeta,
    ModelCheckpoint,
};

pub use online_learner::GasModelOnlineLearner;

pub use ceiling_escalation::{CeilingEscalationState, CeilingEscalationTracker};

pub use validation::{
    AttributionValidator,
    ExecutionTrace,
    ExecutionTraceValidator,
    PipelineLossEvent,
    PipelineLossEventValidator,
    ValidationError,
    ValidationResult,
    Validator,
};

pub use dashboard::{AttributionDashboard, DashboardSnapshot};