// crates/omega-core/src/lib.rs
//
// omega-core — zero-dependency type foundation for the Omega Engine.
//
// ## Architectural role (§22.1)
//
// omega-core sits at the root of the dependency graph — every other
// crate depends on it, so it must depend on nothing in the workspace.
// External dependencies are deliberately minimal:
//
//   alloy-primitives  — EVM primitive types (B256, Address, Bytes, U256)
//   serde             — serialisation derives
//   serde_json        — signal payload encoding
//   async-trait       — StrategyTrait async methods
//   anyhow            — error propagation in trait return types
//   thiserror         — typed errors for core error enums
//   chrono / uuid     — timestamps and IDs
//   tracing           — structured telemetry (zero-cost when disabled)
//   hex               — PositionSnapshot::dedup_key encoding
//
// The full `alloy` crate (transport, RPC, signers) is restricted to
// omega-rpc (§22.1).  omega-core depends only on alloy-primitives.
//
// ## Module map
//
//   chain.rs            — ChainId, UnknownChainId
//   config.rs           — OmegaConfig and sub-configs (GasConfig, LaConfig …)
//   errors.rs           — OmegaError, DropCode
//   types/blueprint.rs  — ExecutionBlueprint, StrategyId
//   types/health.rs     — HealthStatus, LayerId, LayerHealth trait
//   types/lane.rs       — Lane, Simulator
//   types/oracle.rs     — OraclePrice, PositionSnapshot, FeeSnapshot, LaTier
//   types/signal.rs     — OracleSignal, SignalKind
//   types/strategy.rs   — StrategyTrait, OpScore, SimResult, SignalState

pub mod chain;
pub mod config;
pub mod errors;
pub mod types;

// ── Convenience re-exports ────────────────────────────────────────────────────
// Downstream crates can write `use omega_core::ExecutionBlueprint` rather
// than the full module path.
pub use chain::{ChainId, UnknownChainId};
pub use config::{
    ApiConfig, GasConfig, LaConfig, MlConfig, OmegaConfig, RelayConfig, RotationConfig, VaultConfig,
};
pub use errors::{DropCode, OmegaError};
pub use types::{
    ExecutionBlueprint, FeeSnapshot, HealthState, HealthStatus, LaTier, Lane, LayerHealth,
    LayerHealthReport, LayerId, OpScore, OraclePrice, OracleSignal, PositionSnapshot, SignalKind,
    SignalState, SimResult, Simulator, StrategyId, StrategyTrait,
};
