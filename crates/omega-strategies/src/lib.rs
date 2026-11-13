// crates/omega-strategies/src/lib.rs
//
// omega-strategies — concrete StrategyTrait implementations.
//
// ## Architectural role (§22.1)
//
//   omega-strategies depends on omega-core, omega-gas-war,
//   omega-loss-attribution, and omega-flashloan. It does NOT depend on
//   omega-oracle or omega-risk for production data — those are injected
//   via traits (QuoteSource, SandwichOracleSource, MevShareOracleSource,
//   TokenPriceLookup, SimulatorBackend) so this crate stays compilable
//   without the full RPC/oracle stack.
//
// ## Module map
//
//   registry.rs         — StrategyRegistry
//   revm_cache.rs       — EIL double-buffer revm state cache
//   flashloan_select.rs — FlashloanProvider → FlashloanProviderType mapping
//   oracle.rs           — SandwichOracleSource / MevShareOracleSource traits
//   simulator.rs        — SimulatorBackend trait
//   sa.rs               — Simple Arbitrage, Phase 1, Microtx lane
//   cnry.rs             — Canary, Phase 0 signal validator, no capital
//   msa.rs              — Multi-Step Arbitrage, Phase 2, Normal lane
//   la.rs               — Liquidation Arbitrage, Phase 3, Normal lane
//   mev.rs              — MEV-OFA / Backrun, Phase 4, Normal lane

pub mod cnry;
pub mod flashloan_select;
pub mod la;
pub mod mev;
pub mod msa;
pub mod oracle;
pub mod registry;
pub mod revm_cache;
pub mod sa;
pub mod simulator;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use cnry::CnryStrategy;
pub use flashloan_select::to_blueprint_provider_type;
pub use la::{LaStrategy, LiquidationProtocol, TokenPriceLookup};
pub use mev::MevStrategy;
pub use msa::MsaStrategy;
pub use oracle::{
    MevShareOpportunity, MevShareOracleSource, NoOpMevShareOracleSource, NoOpSandwichOracleSource,
    SandwichOpportunity, SandwichOracleSource,
};
pub use registry::StrategyRegistry;
pub use revm_cache::{RevmCacheManager, RevmStateCache};
pub use sa::{QuoteSource, SaStrategy, StaticQuoteSource};
pub use simulator::SimulatorBackend;

#[cfg(any(test, feature = "test-helpers"))]
pub use oracle::{StaticMevShareOracleSource, StaticSandwichOracleSource};
