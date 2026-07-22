// crates/omega-core/src/types/signal.rs
//
// Signal types â€” the data contract between the oracle layer (omega-oracle)
// and the strategy layer (omega-strategies).
//
// Signals are the market observations that drive strategy scoring.
// They cross the EIL (Execution Isolation Layer) double-buffer boundary
// (Â§6) via a lock-free arc-swap channel.  Every signal carries a
// monotonic `state_version` and a `state_hash` so the EIL can detect
// stale state before permitting simulation (Â§6, Â§13.4).
//
// Spec references:
//   Â§2   â€” L0/L1 channel map (oracle â†’ EIL â†’ strategy)
//   Â§6   â€” EIL double-buffer; state_version staleness detection
//   Â§10  â€” MSA Bellman-Ford signal (PriceSignal::PoolReserves)
//   Â§11  â€” LA position health factor (PriceSignal::HealthFactor)
//   Â§13  â€” signal_state_hash is the join key for loss attribution

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// SignalKind
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Discriminates the market signal type carried in an [`OracleSignal`].
///
/// Used by strategy routers to filter signals relevant to their
/// execution path without deserialising the full payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// Spot price update from a DEX oracle (Uniswap v3 TWAP, Chainlink,
    /// Pyth).  Relevant to SA (Phase 1) and MSA (Phase 2).
    SpotPrice,

    /// AMM pool reserve update.  Used by MSA Bellman-Ford graph
    /// construction (Â§10).
    PoolReserves,

    /// Lending position health factor update.  The primary signal for
    /// LA hot/warm/cold tier classification (Â§11.1).
    HealthFactor,

    /// On-chain mempool / order-flow signal.  Used by MEV-OFA (Phase 4).
    OrderFlow,

    /// Block-level fee oracle update (base fee, L1 data fee, priority
    /// fee market).  Fed into the dual-component gas model (Â§7).
    FeeOracle,
}

// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
// OracleSignal
// â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A single oracle observation delivered to the strategy layer.
///
/// The EIL double-buffer swaps a `Arc<Vec<OracleSignal>>` snapshot
/// atomically â€” strategies receive a consistent set of signals for the
/// same `state_version` (Â§6).
///
/// ## Versioning invariant
///
/// `state_version` strictly increases per chain.  A strategy that
/// receives a signal with `state_version â‰¤ blueprint.state_version`
/// must treat its blueprint as stale and abort submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSignal {
    /// Discriminant â€” lets routers skip irrelevant signals cheaply.
    pub kind: SignalKind,

    /// EIP-155 chain ID this signal originates from.
    pub chain_id: u64,

    /// Block number at which this signal was observed on-chain.
    pub block_number: u64,

    /// Timestamp (Unix seconds) when the oracle layer received this
    /// signal.  Used for latency tracking in the Observability layer (Â§16).
    pub received_at_unix_ms: u64,

    /// Monotonically increasing state snapshot version (per chain).
    /// Shared with [`crate::types::strategy::SignalState::state_version`].
    pub state_version: u64,

    /// keccak256 of the canonical serialised oracle state at this
    /// snapshot.  Recorded on blueprints as `signal_state_hash` and
    /// validated by the EIL before simulation (Â§6).
    pub state_hash: B256,

    /// Signal payload â€” strategy-specific.  Encoded as JSON for
    /// cross-crate flexibility; each strategy deserialises into its
    /// own domain types.
    ///
    /// Payload schemas per SignalKind:
    ///   SpotPrice    â†’ `{ "token": "0xâ€¦", "price_usd_e18": "â€¦" }`
    ///   PoolReserves â†’ `{ "pool": "0xâ€¦", "reserve0": "â€¦", "reserve1": "â€¦" }`
    ///   HealthFactor â†’ `{ "position": "0xâ€¦", "hf_e18": "â€¦", "protocol": "aave_v3" }`
    ///   OrderFlow    â†’ `{ "tx_hash": "0xâ€¦", "decoded_swap": { â€¦ } }`
    ///   FeeOracle    â†’ `{ "base_fee_gwei": N, "l1_data_fee_gwei": N, "priority_fee_gwei": N }`
    pub payload: serde_json::Value,
}