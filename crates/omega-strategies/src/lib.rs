// crates/omega-strategies/src/lib.rs
//
// omega-strategies — concrete StrategyTrait implementations.
//
// ## Architectural role (§22.1)
//
//   omega-strategies depends on omega-core, omega-gas-war,
//   omega-loss-attribution, and omega-flashloan (added this revision —
//   see flashloan_select.rs and each strategy's own capital-path notes
//   for why). It does NOT depend on omega-oracle or omega-risk — those
//   stay injected via the StrategyRegistry as trait objects, keeping
//   this crate compilable without the full RPC/oracle stack.
//
// ## Module map
//
//   registry.rs         — StrategyRegistry: maps StrategyId → StrategyTrait,
//                          enforces bytecode hash verification (§8)
//   revm_cache.rs        — EIL double-buffer revm state cache (§6)
//   flashloan_select.rs  — shared FlashloanProvider -> FlashloanProviderType
//                          mapping used by build_blueprint (LA today)
//   sa.rs                — Simple Arbitrage, Phase 1, Microtx lane
//   cnry.rs              — Canary, Phase 0 signal validator, no capital
//   msa.rs               — Multi-Step Arbitrage, Phase 2, Normal lane
//   la.rs                — Liquidation Arbitrage, Phase 3, Normal lane
//   mev.rs               — MEV-OFA / Backrun, Phase 4, Normal lane

pub mod cnry;
pub mod flashloan_select;
pub mod la;
pub mod mev;
pub mod msa;
pub mod registry;
pub mod revm_cache;
pub mod sa;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use cnry::CnryStrategy;
pub use flashloan_select::to_blueprint_provider_type;
pub use la::LaStrategy;
pub use mev::MevStrategy;
pub use msa::MsaStrategy;
pub use registry::StrategyRegistry;
pub use revm_cache::{RevmCacheManager, RevmStateCache};
pub use sa::SaStrategy;