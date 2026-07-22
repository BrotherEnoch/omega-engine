ï»¿// crates/omega-dag/src/lib.rs
//
// omega-dag â€” Execution scheduling DAG for the Omega Engine (spec Â§9).
//
// ## Spec Â§9 â€” DAG
//
//   The DAG (Directed Acyclic Graph) is the central scheduling data
//   structure that manages all in-flight blueprints.  It enforces:
//
//   1. Dependency ordering â€” blueprint B may declare that it depends on
//      blueprint A.  A must complete before B is released for simulation.
//      Circular dependencies â†’ `DropCode::MissDagCycle`.
//
//   2. Slot capacity â€” each execution lane has a bounded concurrency limit:
//        Microtx lane: `DagConfig::microtx_slots` (default 8)
//        Normal lane:  `DagConfig::normal_slots`  (default 16)
//      Canary blueprints do not consume slots (Â§1.1).
//      Exceeding capacity triggers preemptive eviction of the
//      lowest-priority occupant, or `DropCode::MissCapacity` /
//      `DropCode::MissCapacityNormal` if no lower-priority occupant
//      exists.
//
//   3. Priority ordering â€” within each lane, MEV(0) > LA(1) > MSA(2) >
//      SA(3) > CNRY(255).  Higher-priority blueprints may evict
//      lower-priority occupants to claim a slot.
//
// ## Architectural role (Â§22.1)
//
//   omega-dag â† omega-core
//
//   It is a pure synchronous data structure with no I/O.  Callers hold
//   an `ExecutionDag` behind a `Mutex` or in a single-threaded task.
//
// ## Module map
//
//   types.rs     â€” `DagError`, `DagConfig`, `EvictionRecord`, `DagSnapshot`.
//   scheduler.rs â€” `ExecutionDag`: admit, complete, ready, snapshot.

pub mod scheduler;
pub mod types;

#[cfg(test)]
mod tests;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use scheduler::ExecutionDag;
pub use types::{DagConfig, DagError, DagSnapshot, EvictionRecord};