// crates/omega-core/src/lib.rs
//
// omega-core â€” zero-dependency type foundation for the Omega Engine.
//
// ## Architectural role (Â§22.1)
//
// omega-core sits at the root of the dependency graph â€” every other
// crate depends on it, so it must depend on nothing in the workspace.
// External dependencies are deliberately minimal:
//
//   alloy-primitives  â€” EVM primitive types (B256, Address, Bytes, U256)
//   serde             â€” serialisation derives
//   serde_json        â€” signal payload encoding
//   async-trait       â€” StrategyTrait async methods
//   anyhow            â€” error propagation in trait return types
//   thiserror         â€” typed errors for core error enums
//   chrono / uuid     â€” timestamps and IDs
//   tracing           â€” structured telemetry (zero-cost when disabled)
//   hex               â€” PositionSnapshot::dedup_key encoding
//
// The full `alloy` crate (transport, RPC, signers) is restricted to
// omega-rpc (Â§22.1).  omega-core depends only on alloy-primitives.
//
// ## Module map
//
//   chain.rs            â€” ChainId, UnknownChainId
//   config.rs           â€” OmegaConfig and sub-configs (GasConfig, LaConfig â€¦)
//   errors.rs           â€” OmegaError, DropCode
//   types/blueprint.rs  â€” ExecutionBlueprint, StrategyId
//   types/health.rs     â€” HealthState, LayerId, LayerHealth trait
//   types/lane.rs       â€” Lane, Simulator
//   types/oracle.rs     â€” OraclePrice, PositionSnapshot, FeeSnapshot, LaTier
//   types/signal.rs     â€” OracleSignal, SignalKind
//   types/strategy.rs   â€” StrategyTrait, OpScore, SimResult, SignalState

pub mod chain;
pub mod config;
pub mod errors;
pub mod types;

// â”€â”€ Convenience re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// Downstream crates can write `use omega_core::ExecutionBlueprint` rather
// than the full module path.

pub use chain::{ChainId, UnknownChainId};

pub use config::{
    ApiConfig,
    GasConfig,
    LaConfig,
    MlConfig,
    OmegaConfig,
    RelayConfig,
    RotationConfig,
    VaultConfig,
};

pub use errors::{DropCode, OmegaError};

pub use types::{
    ExecutionBlueprint,
    FeeSnapshot,
    HealthState,
    LayerHealth,
    LayerId,
    Lane,
    LaTier,
    OpScore,
    OraclePrice,
    OracleSignal,
    PositionSnapshot,
    SignalKind,
    SignalState,
    SimResult,
    Simulator,
    StrategyId,
    StrategyTrait,
};