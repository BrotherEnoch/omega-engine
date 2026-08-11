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
// omega-hot-path invokes the individual oracle/slippage checks directly
// (it does not run the full 16-check pipeline).
//
// ## Fix (this revision, 2): re-exported the WRONG function names
//
// This file previously did
//   `pub use checks::{check_oracle_freshness, check_oracle_hierarchy, check_slippage, ...}`
// — but those three identifiers name the PRIVATE thin-wrapper functions
// inside checks.rs (`fn check_oracle_freshness(bp: &BlueprintFields, ctx:
// &CheckContext) -> ...`), not the `pub` standalone functions checks.rs
// actually defines for exactly this cross-crate use case (E0603: function
// is private). The real public functions — the ones that take only an
// `&OracleSnapshot` or bare `u16`s, with no `BlueprintFields`/
// `CheckContext` required, which is the whole point for a caller like
// omega-hot-path — are named `oracle_freshness_check`,
// `oracle_hierarchy_check`, `oracle_price_sanity_check`, and
// `slippage_check`. Corrected the re-export list to name those instead,
// and added `oracle_price_sanity_check` (check 16 / MissFlashCrash),
// which was omitted entirely from the previous list despite existing in
// checks.rs and being just as relevant to a caller bypassing the full
// pipeline as the other three.

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
    oracle_freshness_check, oracle_hierarchy_check, oracle_price_sanity_check, run_all_checks,
    slippage_check, BlueprintFields, CheckResult,
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