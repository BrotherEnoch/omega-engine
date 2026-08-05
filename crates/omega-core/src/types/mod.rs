// crates/omega-core/src/types/mod.rs
//
// Public type surface of omega-core.
//
// Visibility policy (§22.1):
//   Every type here is `pub` — omega-core is a pure-types crate with
//   no business logic.  Business logic lives in the crate that owns
//   the relevant specification section (omega-strategies for §11,
//   omega-gas-war for §12, omega-loss-attribution for §13, etc.).
//
//   The only exception is internal helpers that are pub(crate) — these
//   are implementation details of the type constructors within this
//   module.
//
// ## Fix (this revision)
//
// `FlashloanProviderType` was being re-exported from BOTH
// `blueprint::FlashloanProviderType` and
// `flashloan_provider::FlashloanProviderType` — two different enums
// with the same name, an `E0252` conflict (and, worse, two genuinely
// different types silently pretending to be one, depending which
// import path a caller happened to use). `flashloan_provider.rs` is
// this type's real, pre-existing home (see that module's own file);
// `blueprint.rs` was WRONGLY declaring a second, competing definition
// of the same name — that duplicate definition needs to be deleted
// from `blueprint.rs` and replaced with
// `use crate::types::flashloan_provider::FlashloanProviderType;` there
// instead. This file now re-exports only the real one.

pub mod blueprint;
pub mod flashloan_provider;
pub mod health;
pub mod lane;
pub mod oracle;
pub mod signal;
pub mod strategy;
pub mod strategy_registry_key;

// Convenience re-exports of the most commonly used types so downstream
// crates can write `use omega_core::types::ExecutionBlueprint` rather
// than the full module path.
pub use blueprint::{ExecutionBlueprint, StrategyId};
pub use flashloan_provider::FlashloanProviderType;
pub use health::{HealthState, HealthStatus, LayerHealth, LayerHealthReport, LayerId};
pub use lane::{Lane, Simulator};
pub use oracle::{FeeSnapshot, LaTier, OraclePrice, PositionSnapshot};
pub use signal::{OracleSignal, SignalKind};
pub use strategy::{OpScore, SignalState, SimResult, StrategyTrait};

// `strategy_registry_key` intentionally has no re-export line: it adds
// only an inherent impl (`StrategyId::to_registry_key`) on the
// already-re-exported `StrategyId`, and defines no new public type of
// its own that would need re-exporting.