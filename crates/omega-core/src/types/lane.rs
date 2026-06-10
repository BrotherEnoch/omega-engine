// crates/omega-core/src/types/lane.rs
// crates/omega-core/src/types/lane.rs
//
// Lane and Simulator enums.
//
// Lane drives the selection of simulation backend (§1.1, §4):
//   Microtx  → revm  (zero-copy, <200k gas, Phase 1 SA hot-path)
//   Normal   → Anvil (full EVM fork, MSA / LA / MEV)
//
// Simulator is recorded on ExecutionBlueprint so downstream crates
// (omega-strategies, omega-hot-path) know exactly which backend was
// used during blueprint construction without re-inspecting gas figures.

use serde::{Deserialize, Serialize};

/// Execution lane — determines throughput tier and simulation backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Sub-200k-gas transactions routed through the revm micro-execution
    /// path.  Zero-copy, CPU-pinned, targets <1ms simulation latency.
    /// Used by Phase 1 SA and Phase 3 LA hot-path (§4, §11).
    Microtx,
    /// Full EVM fork via Anvil.  Used by MSA (Phase 2), LA warm/cold
    /// tier, and MEV (Phase 4).  Higher latency but handles arbitrary
    /// state complexity.
    Normal,
}

/// Simulation backend chosen for a blueprint.
///
/// Recorded on [`ExecutionBlueprint`] at construction time.  Downstream
/// consumers (relay submission, loss attribution) use this to correlate
/// simulation results with the correct error sub-classification
/// (§13.4 SIMULATION_STATE_MISMATCH vs SIMULATION_GAS_MISCALC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Simulator {
    /// revm in-process execution.  Fast, deterministic, relies on the
    /// EIL double-buffer cache (§6).  Cache staleness produces
    /// SIMULATION_STATE_MISMATCH losses (§13.4).
    Revm,
    /// Anvil fork.  Full node state — no cache staleness risk.
    /// Higher latency (~50–200ms).  Used when Microtx lane is
    /// inappropriate or gas estimate exceeds 200k.
    Anvil,
}
