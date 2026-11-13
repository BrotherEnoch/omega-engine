// crates/omega-strategies/src/oracle.rs
//
// Oracle source traits for MSA and MEV strategies.
//
// ## Wiring
//
// 1. Add `pub mod oracle;` to crates/omega-strategies/src/lib.rs.
// 2. Implement these traits in `omega-oracle` (or main.rs adapters).
// 3. Inject `Arc<dyn SandwichOracleSource>` / `Arc<dyn MevShareOracleSource>`
//    into `MsaStrategy::new` and `MevStrategy::new_with_registry` at startup.
//
// ## Production implementations
//
// - `SandwichOracleSource`: omega-oracle mempool / MEV-Share scanner that
//   scores multi-hop swap targets and surfaces the best sandwich opportunity.
//
// - `MevShareOracleSource`: omega-oracle MEV-Share SSE client that parses
//   hint payloads (token, percent fill, block proximity), computes adverse
//   selection, and surfaces the best backrun opportunity each cycle.

#![allow(unexpected_cfgs)]

use alloy_primitives::{Address, B256, U256};

// ─────────────────────────────────────────────────────────────────────────────
// MSA oracle types
// ─────────────────────────────────────────────────────────────────────────────

/// A live sandwich opportunity sourced from the oracle.
///
/// All fields are from real parsed oracle data — no proxies or estimates.
#[derive(Debug, Clone)]
pub struct SandwichOpportunity {
    /// Gross sandwich profit in ETH wei (before gas deduction).
    pub gross_profit_wei: U256,

    /// Target pending transaction hash being sandwiched.
    pub target_tx_hash: B256,

    /// DEX pool addresses for each sandwich hop, in execution order.
    /// Must have at least one entry.
    pub pool_addresses: Vec<Address>,

    /// Token path: [tokenIn, intermediate..., tokenOut].
    /// Length = pool_addresses.len() + 1.
    pub token_path: Vec<Address>,

    /// Front-run swap amounts for each hop, in token_path units.
    pub front_run_amounts: Vec<U256>,

    /// Back-run swap amounts for each hop (reverse of front-run).
    pub back_run_amounts: Vec<U256>,
}

/// Source of live sandwich opportunities.
///
/// Implemented by omega-oracle's mempool scanner. `MsaStrategy` calls
/// `current_opportunity()` on every `score()` and `build_blueprint()` cycle.
/// Returns `None` when no qualifying pending transaction is currently tracked.
///
/// Must return real, freshly-parsed data — no fabricated proxies. Two calls
/// within the same cycle may differ if a new block/hint arrived; callers treat
/// each result as a snapshot (same model as `SignalState`).
pub trait SandwichOracleSource: Send + Sync {
    fn current_opportunity(&self) -> Option<SandwichOpportunity>;
}

/// No-op — always returns `None`. Use at startup before the oracle is connected
/// and in tests that only exercise fail-closed paths.
pub struct NoOpSandwichOracleSource;

impl SandwichOracleSource for NoOpSandwichOracleSource {
    fn current_opportunity(&self) -> Option<SandwichOpportunity> {
        None
    }
}

/// Fixed-opportunity source for unit tests.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone)]
pub struct StaticSandwichOracleSource(pub SandwichOpportunity);

#[cfg(any(test, feature = "test-helpers"))]
impl SandwichOracleSource for StaticSandwichOracleSource {
    fn current_opportunity(&self) -> Option<SandwichOpportunity> {
        Some(self.0.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MEV oracle types
// ─────────────────────────────────────────────────────────────────────────────

/// A live MEV-Share backrun opportunity sourced from the oracle.
///
/// All fields are parsed from the real MEV-Share SSE hint payload — no
/// proxies, no hash-byte tricks.
#[derive(Debug, Clone)]
pub struct MevShareOpportunity {
    /// Adverse selection score in [0.0, 1.0].
    ///
    /// High scores (> 0.5) indicate informed flow likely to move the market
    /// against the backrun (spec §8).
    pub adverse_selection_score: f64,

    /// Expected price-impact profit from the backrun in ETH wei.
    pub price_impact_wei: U256,

    /// User's pending transaction hash. The backrun bundle references this
    /// and must land in the same block.
    pub target_tx_hash: B256,

    /// DEX pool addresses for the backrun hops.
    pub pool_addresses: Vec<Address>,

    /// Token path for the backrun: [tokenIn, intermediate..., tokenOut].
    /// Length = pool_addresses.len() + 1.
    pub token_path: Vec<Address>,

    /// Backrun swap amounts for each hop, in token_path units.
    pub backrun_amounts: Vec<U256>,
}

/// Source of live MEV-Share backrun opportunities.
///
/// Implemented by omega-oracle's MEV-Share SSE client. `MevStrategy` calls
/// `current_opportunity()` on every `score()` and `build_blueprint()` cycle.
pub trait MevShareOracleSource: Send + Sync {
    fn current_opportunity(&self) -> Option<MevShareOpportunity>;
}

/// No-op — always returns `None`.
pub struct NoOpMevShareOracleSource;

impl MevShareOracleSource for NoOpMevShareOracleSource {
    fn current_opportunity(&self) -> Option<MevShareOpportunity> {
        None
    }
}

/// Fixed-opportunity source for unit tests.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug, Clone)]
pub struct StaticMevShareOracleSource(pub MevShareOpportunity);

#[cfg(any(test, feature = "test-helpers"))]
impl MevShareOracleSource for StaticMevShareOracleSource {
    fn current_opportunity(&self) -> Option<MevShareOpportunity> {
        Some(self.0.clone())
    }
}
