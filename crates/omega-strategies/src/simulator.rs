// crates/omega-strategies/src/simulator.rs
//
// Shared simulation backend trait.
//
// All strategies hold `Option<Arc<dyn SimulatorBackend>>`. When Some,
// `StrategyTrait::simulate()` delegates here — the real implementation
// lives in `omega-simulation` (revm, for SA) and `omega-shadow` (Anvil
// fork, for MSA/MEV/LA). When None, `simulate()` returns an error so
// no blueprint reaches the relay without actual simulation.
//
// ## Wiring
//
// 1. Add `pub mod simulator;` to crates/omega-strategies/src/lib.rs.
// 2. In main.rs (or the simulation crate), implement this trait once per
//    backend and pass `Arc<dyn SimulatorBackend>` into each strategy
//    constructor that needs it.

use anyhow::Result;
use async_trait::async_trait;

use omega_core::types::blueprint::ExecutionBlueprint;
use omega_core::types::strategy::SimResult;

/// Injectable simulation backend for strategy `simulate()` calls.
///
/// Implement this in `omega-simulation` (revm) for Microtx/SA strategies
/// and in `omega-shadow` (Anvil fork) for Normal-lane strategies (LA, MSA, MEV).
/// Pass `Option<Arc<dyn SimulatorBackend>>` via each strategy's constructor.
///
/// Contract:
/// - Return `Ok(SimResult { success: false, .. })` on a simulation revert.
/// - Return `Ok(SimResult { success: true, profit_net, gas_used, .. })` only
///   when the on-fork / revm execution succeeds and profit is verified.
/// - Return `Err` only on infrastructure failure (fork spawn failed, RPC
///   unreachable, etc.) — not on a simulation revert.
///
/// When no backend is injected (`None`), strategy `simulate()` methods
/// return an error so no blueprint proceeds to the relay without a real
/// simulation result.
#[async_trait]
pub trait SimulatorBackend: Send + Sync {
    /// Run the blueprint against the simulator and return a real `SimResult`.
    async fn simulate(&self, bp: &ExecutionBlueprint) -> Result<SimResult>;
}
