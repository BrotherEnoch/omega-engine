// crates/omega-risk/src/lib.rs

pub mod gas_model;
pub mod checks;
pub mod context;
pub mod whitelist;
pub mod competition;
pub mod circuit_breakers;
pub mod flash_crash;
pub mod kill_switch;
pub mod heartbeat;
pub mod metrics;

#[cfg(test)]
mod tests;

pub use checks::{run_all_checks, CheckResult, BlueprintFields};
pub use context::CheckContext;
pub use circuit_breakers::{BreakerDiagnostics, CircuitBreakerRegistry, CircuitState};
pub use flash_crash::{FlashCrashGuard, FlashCrashResponse};
pub use kill_switch::{
    KillSwitch, KillSwitchConfig, KillSwitchDiagnostics, KillSwitchRegistry, KillSwitchStatus,
    TripReason as KillSwitchTripReason, WindowLossEntry,
};
pub use heartbeat::{ComponentStatus, HeartbeatConfig, HeartbeatRegistry};
pub use gas_model::{dynamic_min_profit, l1_adaptive_buffer, L2_EXEC_BUFFER, EXTRACTION_GAS};
pub use competition::{competition_probability, priority_fee_gwei, AssetTier};