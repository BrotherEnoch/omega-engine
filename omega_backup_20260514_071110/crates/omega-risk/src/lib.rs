ï»¿// crates/omega-risk/src/lib.rs
//
// OmegaEngine v12.0 â€” omega-risk
//
// Responsibility: pre-trade risk gate for every ExecutionBlueprint.
//
// Spec coverage:
//   Section 5  â€” Risk layer sits between EIL and DAG/ZK in the 14-layer stack.
//   Section 7  â€” Arbitrum dual-component gas model (L2 exec + L1 data adaptive).
//   Section 11 â€” LA-specific checks (price impact, flash-crash guard).
//   Section 12 â€” Gas War Engine: dynamic min-profit, gas spike guard.
//   Section 19 â€” Adaptive EV-weighted rollout: EV ratio monitoring, circuit-breaker.
//   Section 8  â€” Security + adverse selection: per-strategy circuit breakers.
//
// Module layout:
//   gas_model        â€” Arbitrum dual-component gas model + L1 adaptive buffer (EMA).
//   checks           â€” 13 fast-fail pre-trade checks in mandatory spec order.
//   context          â€” CheckContext: all live market state required to run checks.
//   whitelist        â€” Strategy + address whitelist registry.
//   competition      â€” Probabilistic competition score model (per asset-tier + HF).
//   circuit_breakers â€” Per-strategy EV-ratio circuit breakers (spec S19).
//   flash_crash      â€” Graduated flash-crash guard (spec S11 LA).
//   metrics          â€” Prometheus counters/gauges for every check outcome.

pub mod gas_model;
pub mod checks;
pub mod context;
pub mod whitelist;
pub mod competition;
pub mod circuit_breakers;
pub mod flash_crash;
pub mod metrics;

// Re-exports consumed by omega-strategies and omega-gas-war.
pub use checks::{run_all_checks, CheckResult, BlueprintFields};
pub use context::CheckContext;
pub use circuit_breakers::{CircuitBreakerRegistry, CircuitState};
pub use flash_crash::{FlashCrashGuard, FlashCrashResponse};
pub use gas_model::{dynamic_min_profit, l1_adaptive_buffer, L2_EXEC_BUFFER, EXTRACTION_GAS};
pub use competition::{competition_probability, priority_fee_gwei, AssetTier};

#[cfg(test)]
mod tests;