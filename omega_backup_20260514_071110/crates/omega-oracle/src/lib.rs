ï»¿// crates/omega-oracle/src/lib.rs
//
// omega-oracle â€” Tri-oracle price resolution and EIL signal coordinator.
//
// ## Architectural role (Â§22.1)
//
//   omega-oracle sits between omega-rpc (raw on-chain event streams) and
//   omega-strategies (signal consumers).
//
//   Dependency graph:
//     omega-oracle â† omega-rpc, omega-core, omega-health
//     omega-strategies â† omega-oracle (via OracleSignal broadcast)
//
// ## Per-chain instances
//
//   One `PerChainOracle` runs per active chain.  Chains are fully isolated:
//   Arbitrum One (42161) and Ethereum mainnet (1) have independent signal
//   versioning, staleness thresholds, and EIL snapshots.
//
// ## Tri-oracle resolution (Â§7)
//
//   Primary:   Chainlink on-chain TWAP aggregator (staleness: 45s)
//   Secondary: Pyth off-chain price aggregation  (staleness: 45s)
//   Tertiary:  Uniswap v3 TWAP                  (staleness: 120s)
//
//   Resolution rules (priority order) â€” see resolution.rs for full spec.
//
// ## Signal pipeline
//
//   omega-rpc broadcast channels (FeeOracleEvent, DexSyncEvent,
//   LendingProtocolEvent) â†’ `PerChainOracle` update loops â†’
//   `OracleSignal` broadcast â†’ strategy scoring loops.
//
//   The EIL double-buffer (`ArcSwap<EilSnapshot>`) holds the latest
//   consistent snapshot.  Strategies read it lock-free on every oracle tick.
//
// ## Module map
//
//   chainlink.rs   â€” Chainlink on-chain price feed cache and reader
//   pyth.rs        â€” Pyth off-chain price feed cache
//   twap.rs        â€” Uniswap v3 TWAP on-chain reader
//   resolution.rs  â€” Tri-oracle resolution logic (Â§7)
//   la_bonus.rs    â€” Per-asset, per-protocol liquidation bonus oracle (Â§11, Â§12)
//   per_chain.rs   â€” `PerChainOracle`: wires RPC streams â†’ OracleSignal EIL

pub mod chainlink;
pub mod la_bonus;
pub mod per_chain;
pub mod pyth;
pub mod resolution;
pub mod twap;

// â”€â”€ Re-exports â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

pub use chainlink::ChainlinkOracle;
pub use la_bonus::{LaBonusOracle, LendingProtocol};
pub use per_chain::{EilSnapshot, PerChainOracle};
pub use pyth::PythOracle;
pub use resolution::{
    resolve_price, OraclePrice, OracleSource,
    DIVERGENCE_THRESHOLD, PRIMARY_STALE_SECS, TWAP_STALE_SECS,
};
pub use twap::TwapOracle;