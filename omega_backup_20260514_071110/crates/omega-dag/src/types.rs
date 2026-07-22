// crates/omega-dag/src/types.rs
//
// Shared types for the execution DAG (spec Â§9).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use omega_core::types::lane::Lane;

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// DagError
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Error returned when a blueprint cannot be admitted to the DAG.
#[derive(Debug, thiserror::Error)]
pub enum DagError {
    /// Admitting this blueprint would create a dependency cycle (Â§9).
    /// Caller must record `DropCode::MissDagCycle`.
    #[error("DAG cycle detected: blueprint {0} creates a dependency cycle")]
    Cycle(String),

    /// Lane is full and no lower-priority blueprint exists to evict.
    /// Caller must record `DropCode::MissCapacity` / `MissCapacityNormal`.
    #[error("Lane {lane:?} is full ({capacity} slots); no lower-priority blueprint to evict")]
    LaneFull { lane: Lane, capacity: usize },

    /// Canary blueprints must never reach the execution DAG.
    #[error("Canary blueprint must not be admitted to the execution DAG")]
    CanaryBlueprint,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// DagConfig
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Capacity and logging configuration for the execution DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagConfig {
    /// Maximum concurrent blueprints in the Microtx lane.
    /// Spec Â§4: target <1ms simulation latency; high-throughput path.
    pub microtx_slots: usize,

    /// Maximum concurrent blueprints in the Normal (Anvil) lane.
    pub normal_slots: usize,

    /// Maximum entries retained in the eviction log.
    /// Oldest entries are pruned when the log exceeds this limit.
    pub eviction_log_capacity: usize,
}

impl Default for DagConfig {
    fn default() -> Self {
        Self {
            microtx_slots:         32,
            normal_slots:          8,
            eviction_log_capacity: 1_000,
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// EvictionRecord
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A single eviction event, persisted in the eviction log.
///
/// Used by the shadow scorecard `dag_eviction_rate` metric (Â§shadow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvictionRecord {
    /// Blueprint hash of the evicted blueprint (hex).
    pub evicted_hash:  String,
    /// Strategy of the evicted blueprint.
    pub evicted_strat: String,
    /// Blueprint hash of the blueprint that caused the eviction.
    pub caused_by:     String,
    /// Drop code applied to the evicted blueprint.
    pub drop_code:     String,
    /// Execution lane where the eviction occurred.
    pub lane:          String,
    /// UTC timestamp.
    pub timestamp:     DateTime<Utc>,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// DagSnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Point-in-time DAG state for the control-plane observability API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagSnapshot {
    /// Blueprints currently admitted to the Microtx lane.
    pub microtx_used:         usize,
    /// Microtx lane capacity.
    pub microtx_capacity:     usize,
    /// Blueprints currently admitted to the Normal lane.
    pub normal_used:          usize,
    /// Normal lane capacity.
    pub normal_capacity:      usize,
    /// Total blueprints admitted since DAG creation.
    pub total_admitted:       u64,
    /// Total blueprints evicted since DAG creation.
    pub total_evicted:        u64,
    /// Total blueprints dropped due to cycle detection.
    pub total_cycle_drops:    u64,
    /// Total blueprints dropped due to lane capacity.
    pub total_capacity_drops: u64,
    /// Eviction rate per 1,000 blueprints processed.
    pub eviction_rate_per_1000: f64,
    /// Snapshot timestamp.
    pub timestamp:            DateTime<Utc>,
}