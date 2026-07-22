// crates/omega-core/src/types/oracle.rs
//
// Oracle domain types used across the Omega crate graph.
//
// These types represent the processed, validated oracle data that the
// oracle layer (omega-oracle) exposes to strategy scoring.  They are
// distinct from raw OracleSignal payloads (types/signal.rs) â€” signals
// are the wire format; these are the domain model.
//
// Spec references:
//   Â§7   â€” dual-component gas model: FeeSnapshot fields
//   Â§11  â€” LA tier classification: PositionSnapshot health factor
//   Â§11.1 â€” hot/warm/cold/archived tier thresholds
//   Â§11.4 â€” reorg guard: PositionSnapshot.block_number used to detect
//            orphaned blueprints

use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OraclePrice
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Spot price for a single token, sourced and validated by omega-oracle.
///
/// Prices are expressed as 18-decimal fixed-point integers (e18) to
/// avoid floating-point precision loss in profit calculations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePrice {
    /// Token contract address.
    pub token: Address,

    /// Price in USD Ã— 10^18.  Zero indicates the price feed is stale
    /// or unavailable â€” strategies must reject zero-priced tokens.
    pub price_usd_e18: U256,

    /// Block number at which this price was last observed.
    pub block_number: u64,

    /// Unix timestamp (ms) when oracle-layer received this update.
    pub received_at_unix_ms: u64,

    /// Whether this price came from a primary (on-chain TWAP) or
    /// fallback (Chainlink / Pyth) source.  Strategies may apply a
    /// confidence discount on fallback-sourced prices.
    pub is_fallback: bool,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// LaTier
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Liquidation Arbitrage monitoring tier for a lending position (Â§11.1).
///
/// Tier drives the recompute frequency and resource allocation:
///
/// | Tier     | HF range      | Update trigger                          |
/// |----------|---------------|-----------------------------------------|
/// | Hot      | < 1.01        | Every oracle update â€” immediate         |
/// | Warm     | 1.01 â€“ 1.05   | Batched every 200ms OR move > 0.5%     |
/// | Cold     | 1.05 â€“ 1.20   | Lazy every 2 s                          |
/// | Archived | > 1.20        | Lazy on 500-block cycle; eviction cand. |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaTier {
    Hot,
    Warm,
    Cold,
    Archived,
}

impl LaTier {
    /// Classify a position by its health factor (18-decimal fixed-point).
    ///
    /// `hf_e18` is the health factor Ã— 10^18 as stored on Aave/Compound/
    /// Morpho.  Thresholds match Â§11.1 exactly:
    ///   Hot      < 1.01 Ã— 10^18
    ///   Warm     1.01â€“1.05 Ã— 10^18
    ///   Cold     1.05â€“1.20 Ã— 10^18
    ///   Archived > 1.20 Ã— 10^18
    pub fn from_hf_e18(hf_e18: U256) -> Self {
        // 1e18 = 1.0 as a fixed-point multiplier
        const E18: u128 = 1_000_000_000_000_000_000;

        // Thresholds in e18 notation
        let hot_threshold  = U256::from(E18 + E18 / 100);           // 1.01e18
        let warm_threshold = U256::from(E18 + 5 * E18 / 100);       // 1.05e18
        let cold_threshold = U256::from(E18 + 20 * E18 / 100);      // 1.20e18

        if hf_e18 < hot_threshold {
            LaTier::Hot
        } else if hf_e18 < warm_threshold {
            LaTier::Warm
        } else if hf_e18 < cold_threshold {
            LaTier::Cold
        } else {
            LaTier::Archived
        }
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// PositionSnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Snapshot of a single lending position for LA scoring (Â§11).
///
/// Produced by omega-oracle from on-chain position data (Aave v3
/// `getUserAccountData`, Compound `getAccountLiquidity`, etc.) and
/// cached in the EIL double-buffer.
///
/// ## Key invariant
///
/// A blueprint built from a PositionSnapshot is only valid while the
/// snapshot's `block_number` is within the revm trust window (Â§6).
/// The LA reorg guard (Â§11.4) tracks `block_number` to detect orphaned
/// blueprints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionSnapshot {
    /// Borrower's wallet address.
    pub borrower: Address,

    /// Lending protocol contract address (Aave v3 pool, Compound
    /// comptroller, Morpho, Euler v2).
    pub protocol: Address,

    /// Health factor Ã— 10^18.  Below 1e18 â†’ liquidatable.
    pub hf_e18: U256,

    /// Total collateral value in USD Ã— 10^18.
    pub collateral_usd_e18: U256,

    /// Total debt value in USD Ã— 10^18.
    pub debt_usd_e18: U256,

    /// Liquidation bonus in basis points (e.g. 500 = 5%).
    pub liquidation_bonus_bps: u16,

    /// Monitoring tier derived from `hf_e18` at snapshot time (Â§11.1).
    pub tier: LaTier,

    /// Block number at which this snapshot was taken.
    /// Used by the reorg guard (Â§11.4) and sequencer restart handler
    /// (Â§11.3) as the deduplication anchor.
    pub block_number: u64,

    /// Monotonic state version from the EIL (Â§6).
    pub state_version: u64,
}

impl PositionSnapshot {
    /// Stable deduplication key for the sequencer restart DashMap (Â§11.3).
    ///
    /// Key = keccak256(borrower ++ protocol) encoded as hex.
    /// Does NOT include block_number so that the same position is
    /// deduplicated across blocks within the 60-block restart window.
    pub fn dedup_key(&self) -> String {
        use alloy_primitives::keccak256;
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(self.borrower.as_slice());
        buf.extend_from_slice(self.protocol.as_slice());
        hex::encode(keccak256(&buf).as_slice())
    }

    /// Returns `true` when this position is currently liquidatable
    /// (health factor below 1.0 Ã— 10^18).
    #[inline]
    pub fn is_liquidatable(&self) -> bool {
        const E18: u128 = 1_000_000_000_000_000_000;
        self.hf_e18 < U256::from(E18)
    }
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// FeeSnapshot
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Current Arbitrum fee oracle reading (Â§7, Â§12.2).
///
/// Both components are required for the dual-component gas model:
///   total_gas_cost = (l2_exec_gas Ã— base_fee) + (l1_data_gas Ã— l1_data_fee)
///
/// Priority fee (tip) is submitted to the Arbitrum sequencer separately
/// and does not affect the gas cost calculation â€” it affects inclusion
/// probability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeSnapshot {
    /// EIP-1559 base fee in gwei (L2 execution cost component).
    pub base_fee_gwei: u64,

    /// L1 data fee per gas unit in gwei (calldata cost component).
    /// Sourced from Arbitrum's ArbGasInfo precompile.
    pub l1_data_fee_gwei: u64,

    /// Current competitive priority fee in gwei (Â§12.2).
    /// The Gas War Engine uses this to set `priority_fee_gwei` on
    /// blueprints, bounded by the 500 gwei ceiling.
    pub priority_fee_gwei: u64,

    /// Block number at which this fee oracle was sampled.
    pub block_number: u64,
}