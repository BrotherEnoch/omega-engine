// crates/omega-dag/src/scheduler.rs
//
// ExecutionDag — blueprint scheduling DAG (spec §9).
//
// ## Spec §9 — DAG and dependency resolution
//
//   The ExecutionDag enforces three invariants across all in-flight
//   blueprints:
//
//   1. CYCLE DETECTION
//      When a blueprint declares dependencies on other in-flight
//      blueprints (e.g. an MSA hop depends on an SA swap completing
//      first), the DAG checks for cycles via Tarjan's SCC before
//      admitting the node.  A cycle → `DropCode::MissDagCycle`.
//
//   2. SLOT CAPACITY
//      Microtx lane: `DagConfig::microtx_slots` concurrent nodes.
//      Normal lane:  `DagConfig::normal_slots`  concurrent nodes.
//      CNRY blueprints do not consume slots (they are informational).
//      Exceeding capacity → `DropCode::MissCapacity` (Microtx) or
//      `DropCode::MissCapacityNormal` (Normal).
//
//   3. PRIORITY ORDERING
//      Within each lane, blueprints are scheduled by `StrategyId::priority`
//      (MEV=0 > LA=1 > MSA=2 > SA=3).  Lower priority blueprints are
//      evicted first when a higher-priority blueprint needs a slot
//      (spec §9: "preemptive eviction").
//
// ## Data model
//
//   Nodes: each admits one `ExecutionBlueprint` identified by `blueprint_hash`.
//   Edges: directed dependency edges A → B mean "A must complete before B".
//   The graph is stored in a `petgraph::stable_graph::StableDiGraph` so that
//   node removal (eviction) does not invalidate other indices.
//
// ## Thread safety
//
//   `ExecutionDag` uses `&mut self` on all mutating methods.  The caller
//   (orchestrator) is responsible for wrapping it in a `Mutex` or
//   confining it to a single task.  This avoids double-locking and
//   keeps the scheduler lock-free in the common path.
//
// ## types.rs alignment notes
//
//   DagError has three variants: Cycle(String), LaneFull { lane, capacity },
//   CanaryBlueprint.  scheduler.rs maps its richer error semantics onto
//   these:
//     - duplicate hash            → Cycle("duplicate: {hash}")
//     - missing dependency        → Cycle("dep not found: {dep}")
//     - lane at capacity          → LaneFull { lane, capacity }
//     - no eviction candidate     → LaneFull { lane, capacity }
//
//   EvictionRecord uses String fields (evicted_strat, caused_by, drop_code)
//   rather than typed u8/Lane — priority values and lane are formatted as
//   strings when constructing the record.
//
//   DagSnapshot tracks total_admitted, total_evicted, total_cycle_drops,
//   total_capacity_drops, eviction_rate_per_1000, and timestamp; the
//   scheduler maintains matching counters in ExecutionDag state.

use std::collections::{HashMap, VecDeque};

use alloy_primitives::B256;
use chrono::Utc;
use petgraph::Direction;
use petgraph::algo::is_cyclic_directed;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use omega_core::errors::{DropCode, OmegaError};
use omega_core::types::blueprint::ExecutionBlueprint;
use omega_core::types::lane::Lane;

use crate::types::{DagConfig, DagError, DagSnapshot, EvictionRecord};

// ─────────────────────────────────────────────────────────────────────────────
// DagNode
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct DagNode {
    blueprint: ExecutionBlueprint,

    #[allow(dead_code)]
    admitted_at: chrono::DateTime<chrono::Utc>,
}

// ─────────────────────────────────────────────────────────────────────────────
// ExecutionDag
// ─────────────────────────────────────────────────────────────────────────────

/// The execution scheduling DAG (spec §9).
///
/// Held behind a `Mutex` in the orchestrator task.
pub struct ExecutionDag {
    config: DagConfig,
    graph: StableDiGraph<DagNode, ()>,
    /// Fast lookup: blueprint_hash → NodeIndex.
    index: HashMap<B256, NodeIndex>,
    microtx_count: usize,
    normal_count: usize,
    /// Monotonically increasing total blueprints admitted.
    total_admitted: u64,
    /// Monotonically increasing total blueprints evicted.
    total_evicted: u64,
    /// Total blueprints dropped due to cycle detection.
    total_cycle_drops: u64,
    /// Total blueprints dropped due to lane capacity.
    total_capacity_drops: u64,
    /// Eviction audit log (bounded to `config.eviction_log_capacity`).
    evictions: VecDeque<EvictionRecord>,
}

impl ExecutionDag {
    // ── Constructor ───────────────────────────────────────────────────────

    pub fn new(config: DagConfig) -> Self {
        Self {
            config,
            graph: StableDiGraph::new(),
            index: HashMap::new(),
            microtx_count: 0,
            normal_count: 0,
            total_admitted: 0,
            total_evicted: 0,
            total_cycle_drops: 0,
            total_capacity_drops: 0,
            evictions: VecDeque::new(),
        }
    }

    // ── Admitting blueprints ──────────────────────────────────────────────

    /// Attempt to admit a blueprint into the DAG.
    ///
    /// ## Steps
    ///
    /// 1. Reject duplicates (same blueprint_hash already in-flight).
    /// 2. Validate all declared dependencies exist in the graph.
    /// 3. Temporarily add the node and edges; check for cycles.
    ///    On cycle: remove the node, return `DagError::Cycle`.
    /// 4. Check slot capacity.  If the lane is full:
    ///    a. Attempt preemptive eviction of the lowest-priority node.
    ///    b. If no lower-priority node exists, return `DagError::LaneFull`.
    /// 5. Record the node; update lane counts.
    ///
    /// CNRY blueprints skip slot capacity checks (§1.1).
    pub fn admit(
        &mut self,
        blueprint: ExecutionBlueprint,
        dependencies: &[B256],
    ) -> Result<(), DagError> {
        let hash = blueprint.blueprint_hash;

        // ── 1. Duplicate check ────────────────────────────────────────────
        // DagError has no Duplicate variant — map to Cycle with a descriptive
        // message so the orchestrator's to_omega_error() → MissDagCycle path
        // is correct.
        if self.index.contains_key(&hash) {
            tracing::warn!(blueprint_hash = %hash, "DAG duplicate blueprint rejected");
            self.total_cycle_drops += 1;
            return Err(DagError::Cycle(format!("duplicate blueprint hash: {hash}")));
        }

        // ── 2. Dependency existence check ─────────────────────────────────
        // DagError has no DependencyNotFound variant — map to Cycle.
        for dep in dependencies {
            if !self.index.contains_key(dep) {
                tracing::warn!(
                    blueprint_hash = %hash,
                    dep            = %dep,
                    "DAG dependency not found — blueprint rejected",
                );
                self.total_cycle_drops += 1;
                return Err(DagError::Cycle(format!(
                    "dependency not found in DAG: {dep} (required by {hash})"
                )));
            }
        }

        // ── 3. Cycle detection ────────────────────────────────────────────
        let node_idx = self.graph.add_node(DagNode {
            blueprint: blueprint.clone(),
            admitted_at: Utc::now(),
        });

        for dep in dependencies {
            let dep_idx = self.index[dep];
            self.graph.add_edge(dep_idx, node_idx, ());
        }

        if is_cyclic_directed(&self.graph) {
            self.graph.remove_node(node_idx);
            self.total_cycle_drops += 1;
            tracing::warn!(
                blueprint_hash = %hash,
                dep_count      = dependencies.len(),
                "DAG cycle detected — blueprint rejected (DropCode::MissDagCycle)",
            );
            // DagError::Cycle is a tuple variant: Cycle(String)
            return Err(DagError::Cycle(format!(
                "{hash} creates a dependency cycle"
            )));
        }

        // ── 4. Slot capacity check ────────────────────────────────────────
        let is_canary = blueprint.strategy_id.is_canary();

        if !is_canary {
            let over_capacity = match blueprint.lane {
                Lane::Microtx => self.microtx_count >= self.config.microtx_slots,
                Lane::Normal => self.normal_count >= self.config.normal_slots,
            };

            if over_capacity {
                let lane_capacity = match blueprint.lane {
                    Lane::Microtx => self.config.microtx_slots,
                    Lane::Normal => self.config.normal_slots,
                };

                match self.try_evict_for(
                    blueprint.lane,
                    blueprint.strategy_id.priority(),
                    &blueprint.strategy_id.to_string(),
                    &hash,
                ) {
                    Ok(evicted_hash) => {
                        tracing::debug!(
                            evicted  = %evicted_hash,
                            admitted = %hash,
                            "Preemptive DAG eviction to admit higher-priority blueprint",
                        );
                    }
                    Err(_) => {
                        self.graph.remove_node(node_idx);
                        self.total_capacity_drops += 1;
                        tracing::debug!(
                            blueprint_hash = %hash,
                            lane           = ?blueprint.lane,
                            capacity       = lane_capacity,
                            "DAG capacity full — blueprint rejected",
                        );
                        // DagError::LaneFull is the closest variant to CapacityFull
                        return Err(DagError::LaneFull {
                            lane: blueprint.lane,
                            capacity: lane_capacity,
                        });
                    }
                }
            }
        }

        // ── 5. Commit ─────────────────────────────────────────────────────
        self.index.insert(hash, node_idx);
        self.total_admitted += 1;
        if !is_canary {
            match blueprint.lane {
                Lane::Microtx => self.microtx_count += 1,
                Lane::Normal => self.normal_count += 1,
            }
        }

        tracing::debug!(
            blueprint_hash = %hash,
            lane           = ?blueprint.lane,
            strategy       = %blueprint.strategy_id,
            priority       = blueprint.strategy_id.priority(),
            microtx_slots  = self.microtx_count,
            normal_slots   = self.normal_count,
            "Blueprint admitted to DAG",
        );

        Ok(())
    }

    // ── Completing / removing nodes ───────────────────────────────────────

    /// Mark a blueprint as complete and remove it from the DAG.
    ///
    /// Returns the set of blueprints that are now unblocked (all their
    /// dependencies have completed).
    pub fn complete(&mut self, hash: B256) -> Vec<B256> {
        let Some(node_idx) = self.index.remove(&hash) else {
            tracing::warn!(blueprint_hash = %hash, "complete() called for unknown blueprint");
            return vec![];
        };

        let successors: Vec<NodeIndex> = self
            .graph
            .neighbors_directed(node_idx, Direction::Outgoing)
            .collect();

        let unblocked: Vec<B256> = successors
            .iter()
            .filter(|&&succ| {
                self.graph
                    .neighbors_directed(succ, Direction::Incoming)
                    .all(|pred| pred == node_idx)
            })
            .filter_map(|&succ| {
                self.graph
                    .node_weight(succ)
                    .map(|n| n.blueprint.blueprint_hash)
            })
            .collect();

        if let Some(node) = self.graph.node_weight(node_idx) {
            let bp = &node.blueprint;
            if !bp.strategy_id.is_canary() {
                match bp.lane {
                    Lane::Microtx => self.microtx_count = self.microtx_count.saturating_sub(1),
                    Lane::Normal => self.normal_count = self.normal_count.saturating_sub(1),
                }
            }
        }

        self.graph.remove_node(node_idx);

        tracing::debug!(
            blueprint_hash  = %hash,
            unblocked_count = unblocked.len(),
            "Blueprint completed; removed from DAG",
        );

        unblocked
    }

    // ── Queries ───────────────────────────────────────────────────────────

    /// Returns all blueprints ready to execute (zero unmet dependencies).
    pub fn ready(&self) -> Vec<B256> {
        self.graph
            .node_indices()
            .filter(|&idx| {
                self.graph
                    .neighbors_directed(idx, Direction::Incoming)
                    .next()
                    .is_none()
            })
            .filter_map(|idx| {
                self.graph
                    .node_weight(idx)
                    .map(|n| n.blueprint.blueprint_hash)
            })
            .collect()
    }

    #[inline]
    pub fn contains(&self, hash: &B256) -> bool {
        self.index.contains_key(hash)
    }

    #[inline]
    pub fn microtx_count(&self) -> usize {
        self.microtx_count
    }

    #[inline]
    pub fn normal_count(&self) -> usize {
        self.normal_count
    }

    #[inline]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn evictions(&self) -> &VecDeque<EvictionRecord> {
        &self.evictions
    }

    /// Produce a serialisable snapshot for the observability API.
    pub fn snapshot(&self) -> DagSnapshot {
        let total_processed =
            self.total_admitted + self.total_cycle_drops + self.total_capacity_drops;
        let eviction_rate = if total_processed > 0 {
            self.total_evicted as f64 / total_processed as f64 * 1_000.0
        } else {
            0.0
        };

        DagSnapshot {
            microtx_used: self.microtx_count,
            microtx_capacity: self.config.microtx_slots,
            normal_used: self.normal_count,
            normal_capacity: self.config.normal_slots,
            total_admitted: self.total_admitted,
            total_evicted: self.total_evicted,
            total_cycle_drops: self.total_cycle_drops,
            total_capacity_drops: self.total_capacity_drops,
            eviction_rate_per_1000: eviction_rate,
            timestamp: Utc::now(),
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Attempt to evict the lowest-priority node in `lane` when it has
    /// lower priority (higher number) than `incoming_priority`.
    ///
    /// `incoming_strat` and `incoming_hash` are used to populate the
    /// `caused_by` field in the `EvictionRecord`.
    fn try_evict_for(
        &mut self,
        lane: Lane,
        incoming_priority: u8,
        incoming_strat: &str,
        incoming_hash: &B256,
    ) -> Result<B256, DagError> {
        let candidate = self
            .graph
            .node_indices()
            .filter_map(|idx| {
                let node = self.graph.node_weight(idx)?;
                let bp = &node.blueprint;
                if bp.lane != lane || bp.strategy_id.is_canary() {
                    return None;
                }
                let prio = bp.strategy_id.priority();
                if prio > incoming_priority {
                    Some((prio, bp.blueprint_hash, bp.strategy_id.to_string(), idx))
                } else {
                    None
                }
            })
            .max_by_key(|(prio, _, _, _)| *prio);

        match candidate {
            Some((evicted_priority, evicted_hash, evicted_strat, node_idx)) => {
                let drop_code = match lane {
                    Lane::Microtx => DropCode::MissCapacity,
                    Lane::Normal => DropCode::MissCapacityNormal,
                };

                // EvictionRecord uses String fields — format typed values
                let record = EvictionRecord {
                    evicted_hash: evicted_hash.to_string(),
                    evicted_strat: format!("{evicted_strat} (prio={evicted_priority})"),
                    caused_by: format!(
                        "{incoming_strat} (prio={incoming_priority}) hash={incoming_hash}"
                    ),
                    drop_code: format!("{drop_code:?}"),
                    lane: format!("{lane:?}"),
                    timestamp: Utc::now(),
                };
                self.evictions.push_back(record);
                if self.evictions.len() > self.config.eviction_log_capacity {
                    self.evictions.pop_front();
                }

                self.index.remove(&evicted_hash);
                self.graph.remove_node(node_idx);
                self.total_evicted += 1;

                match lane {
                    Lane::Microtx => self.microtx_count = self.microtx_count.saturating_sub(1),
                    Lane::Normal => self.normal_count = self.normal_count.saturating_sub(1),
                }

                tracing::info!(
                    evicted_hash      = %evicted_hash,
                    evicted_priority,
                    incoming_priority,
                    lane              = ?lane,
                    "DAG preemptive eviction",
                );

                Ok(evicted_hash)
            }
            None => {
                let capacity = match lane {
                    Lane::Microtx => self.config.microtx_slots,
                    Lane::Normal => self.config.normal_slots,
                };
                // No lower-priority candidate — lane is full with equal/higher priority nodes
                Err(DagError::LaneFull { lane, capacity })
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// OmegaError convenience conversion
// ─────────────────────────────────────────────────────────────────────────────

impl DagError {
    /// Convert to `OmegaError::Dropped` with the appropriate `DropCode` for
    /// the loss attribution pipeline.
    pub fn to_omega_error(&self) -> OmegaError {
        match self {
            // Cycle covers: actual cycle, duplicate hash, missing dependency
            DagError::Cycle(_) => OmegaError::dropped(DropCode::MissDagCycle),
            // LaneFull covers: capacity exceeded + no eviction candidate
            DagError::LaneFull { lane, .. } => match lane {
                Lane::Microtx => OmegaError::dropped(DropCode::MissCapacity),
                Lane::Normal => OmegaError::dropped(DropCode::MissCapacityNormal),
            },
            DagError::CanaryBlueprint => OmegaError::dropped(DropCode::MissDagCycle),
        }
    }
}
