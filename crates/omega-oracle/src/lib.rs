// crates/omega-oracle/src/lib.rs
//
// omega-oracle — Tri-oracle price resolution and EIL signal coordinator.
//
// ## Architectural role (§22.1)
//
//   omega-oracle sits between omega-rpc (raw on-chain event streams) and
//   omega-strategies (signal consumers).
//
//   Dependency graph:
//     omega-oracle ← omega-rpc, omega-core, omega-health
//     omega-strategies ← omega-oracle (via OracleSignal broadcast)
//
// ## Per-chain instances
//
//   One `PerChainOracle` runs per active chain.  Chains are fully isolated:
//   Arbitrum One (42161) and Ethereum mainnet (1) have independent signal
//   versioning, staleness thresholds, and EIL snapshots.
//
// ## Tri-oracle resolution (§7)
//
//   Primary:   Chainlink on-chain TWAP aggregator (staleness: 45s)
//   Secondary: Pyth off-chain price aggregation  (staleness: 45s)
//   Tertiary:  Uniswap v3 TWAP                  (staleness: 120s)
//
//   Resolution rules (priority order) — see resolution.rs for full spec.
//
// ## Signal pipeline
//
//   omega-rpc broadcast channels (FeeOracleEvent, DexSyncEvent,
//   LendingProtocolEvent) → `PerChainOracle` update loops →
//   `OracleSignal` broadcast → strategy scoring loops.
//
//   The EIL double-buffer (`ArcSwap<EilSnapshot>`) holds the latest
//   consistent snapshot.  Strategies read it lock-free on every oracle tick.
//
// ## Module map
//
//   chainlink.rs   — Chainlink on-chain price feed cache and reader
//   pyth.rs        — Pyth off-chain price feed cache
//   twap.rs        — Uniswap v3 TWAP on-chain reader
//   resolution.rs  — Tri-oracle resolution logic (§7)
//   la_bonus.rs    — Per-asset, per-protocol liquidation bonus oracle (§11, §12)
//   per_chain.rs   — `PerChainOracle`: wires RPC streams → OracleSignal EIL

pub mod chainlink;
pub mod la_bonus;
pub mod per_chain;
pub mod pyth;
pub mod resolution;
pub mod twap;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use chainlink::ChainlinkOracle;
pub use la_bonus::{LaBonusOracle, LendingProtocol};
pub use per_chain::{EilSnapshot, PerChainOracle};
pub use pyth::PythOracle;
pub use resolution::{
    resolve_price, OraclePrice, OracleSource, DIVERGENCE_THRESHOLD, PRIMARY_STALE_SECS,
    TWAP_STALE_SECS,
};
pub use twap::TwapOracle;
