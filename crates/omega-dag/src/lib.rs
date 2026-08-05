// crates/omega-dag/src/lib.rs
//
// omega-dag — Execution scheduling DAG for the Omega Engine (spec §9).

pub mod scheduler;
pub mod types;

#[cfg(test)]
mod tests;

pub use scheduler::ExecutionDag;
pub use types::{DagConfig, DagError, DagSnapshot, EvictionRecord};
