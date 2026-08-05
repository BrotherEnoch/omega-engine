// crates/omega-risk/src/lib.rs
//
// ## Audit fix (this revision): lint escalation split + dangling module fix
//
// 1. Added `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]`.
//    Cargo.toml's `[lints.clippy]` table sets unwrap_used/expect_used to
//    "warn" crate-wide (see that file's own audit note for why: this
//    crate's test modules use `.unwrap()` extensively). A manifest-level
//    `[lints]` table has no way to say "deny outside tests, warn inside
//    them" — that split can only be expressed here, as a source-level
//    attribute. `cfg_attr(not(test), deny(...))` does exactly that: when
//    compiling in test mode, this attribute doesn't fire, and the crate
//    falls back to the manifest's "warn"; when compiling normally (i.e.
//    any of this crate's non-test code, which is what actually runs in
//    production), the deny applies. This is the mechanism promised when
//    Cargo.toml's lints were added — without it, "warn" would silently
//    apply everywhere, including the real (non-test) risk-check logic,
//    which was never the intent.
//
// 2. Removed `#[cfg(test)] mod tests;`. This crate has no
//    `src/tests.rs` — confirmed directly — so this line is a compile
//    error on its own, unrelated to anything else in this revision. Every
//    module in this crate (checks.rs, circuit_breakers.rs, kill_switch.rs,
//    flash_crash.rs, whitelist.rs, competition.rs, gas_model.rs,
//    heartbeat.rs) already carries its own inline `#[cfg(test)] mod
//    <name>_tests { ... }`, so an additional top-level `tests` module was
//    never wired to anything and can simply be deleted rather than
//    replaced.
//
// ## Audit fix (this revision): export individual oracle checks
//
// omega-hot-path invokes check_oracle_freshness / check_oracle_hierarchy /
// check_slippage directly (it does not run the full 15-check pipeline).
// Those three functions are therefore re-exported here alongside the
// existing run_all_checks surface.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod checks;
pub mod circuit_breakers;
pub mod competition;
pub mod context;
pub mod flash_crash;
pub mod gas_model;
pub mod heartbeat;
pub mod kill_switch;
pub mod metrics;
pub mod whitelist;

pub use checks::{
    check_oracle_freshness, check_oracle_hierarchy, check_slippage, run_all_checks,
    BlueprintFields, CheckResult,
};
pub use circuit_breakers::{BreakerDiagnostics, CircuitBreakerRegistry, CircuitState};
pub use competition::{competition_probability, priority_fee_gwei, AssetTier};
pub use context::{CheckContext, FlashloanSnapshot, OracleSnapshot};
pub use flash_crash::{FlashCrashGuard, FlashCrashResponse};
pub use gas_model::{dynamic_min_profit, l1_adaptive_buffer, EXTRACTION_GAS, L2_EXEC_BUFFER};
pub use heartbeat::{ComponentStatus, HeartbeatConfig, HeartbeatRegistry};
pub use kill_switch::{
    KillSwitch, KillSwitchConfig, KillSwitchDiagnostics, KillSwitchRegistry, KillSwitchStatus,
    TripReason as KillSwitchTripReason, WindowLossEntry,
};
