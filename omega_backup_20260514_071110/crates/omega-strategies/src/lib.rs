ï»¿// crates/omega-strategies/src/lib.rs
//
// omega-strategies â€” concrete StrategyTrait implementations.
//
// ## Architectural role (Â§22.1)
//
//   omega-strategies depends on omega-core, omega-gas-war, and
//   omega-loss-attribution.  It does NOT depend on omega-oracle,
//   omega-risk, or omega-flashloan â€” those are injected via the
//   StrategyRegistry as trait objects, keeping this crate compilable
//   without the full RPC/oracle stack.
//
// ## Module map
//
//   registry.rs    â€” StrategyRegistry: maps StrategyId â†’ StrategyTrait,
//                    enforces bytecode hash verification (Â§8)
//   revm_cache.rs  â€” EIL double-buffer revm state cache (Â§6)
//   sa.rs          â€” Simple Arbitrage, Phase 1, Microtx lane
//   cnry.rs        â€” Canary, Phase 0 signal validator, no capital
//   msa.rs         â€” Multi-Step Arbitrage, Phase 2, Normal lane
//   la.rs          â€” Liquidation Arbitrage, Phase 3, Normal lane
//   mev.rs         â€” MEV-OFA / Backrun, Phase 4, Normal lane

pub mod cnry;
pub mod la;
pub mod mev;
pub mod msa;
pub mod registry;
pub mod revm_cache;
pub mod sa;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use cnry::CnryStrategy;
pub use la::LaStrategy;
pub use mev::MevStrategy;
pub use msa::MsaStrategy;
pub use registry::StrategyRegistry;
pub use revm_cache::{RevmCacheManager, RevmStateCache};
pub use sa::SaStrategy;