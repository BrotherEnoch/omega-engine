// crates/omega-core/src/types/signal.rs
//
// Signal types — the data contract between the oracle layer (omega-oracle)
// and the strategy layer (omega-strategies).
//
// Signals are the market observations that drive strategy scoring.
// They cross the EIL (Execution Isolation Layer) double-buffer boundary
// (§6) via a lock-free arc-swap channel.  Every signal carries a
// monotonic `state_version` and a `state_hash` so the EIL can detect
// stale state before permitting simulation (§6, §13.4).
//
// Spec references:
//   §2   — L0/L1 channel map (oracle → EIL → strategy)
//   §6   — EIL double-buffer; state_version staleness detection
//   §10  — MSA Bellman-Ford signal (PriceSignal::PoolReserves)
//   §11  — LA position health factor (PriceSignal::HealthFactor)
//   §13  — signal_state_hash is the join key for loss attribution

use alloy_primitives::B256;
use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// SignalKind
// ─────────────────────────────────────────────────────────────────────────────

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
    /// construction (§10).
    PoolReserves,

    /// Lending position health factor update.  The primary signal for
    /// LA hot/warm/cold tier classification (§11.1).
    HealthFactor,

    /// On-chain mempool / order-flow signal.  Used by MEV-OFA (Phase 4).
    OrderFlow,

    /// Block-level fee oracle update (base fee, L1 data fee, priority
    /// fee market).  Fed into the dual-component gas model (§7).
    FeeOracle,
}

// ─────────────────────────────────────────────────────────────────────────────
// OracleSignal
// ─────────────────────────────────────────────────────────────────────────────

/// A single oracle observation delivered to the strategy layer.
///
/// The EIL double-buffer swaps a `Arc<Vec<OracleSignal>>` snapshot
/// atomically — strategies receive a consistent set of signals for the
/// same `state_version` (§6).
///
/// ## Versioning invariant
///
/// `state_version` strictly increases per chain.  A strategy that
/// receives a signal with `state_version ≤ blueprint.state_version`
/// must treat its blueprint as stale and abort submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSignal {
    /// Discriminant — lets routers skip irrelevant signals cheaply.
    pub kind: SignalKind,

    /// EIP-155 chain ID this signal originates from.
    pub chain_id: u64,

    /// Block number at which this signal was observed on-chain.
    pub block_number: u64,

    /// Timestamp (Unix seconds) when the oracle layer received this
    /// signal.  Used for latency tracking in the Observability layer (§16).
    pub received_at_unix_ms: u64,

    /// Monotonically increasing state snapshot version (per chain).
    /// Shared with [`crate::types::strategy::SignalState::state_version`].
    pub state_version: u64,

    /// keccak256 of the canonical serialised oracle state at this
    /// snapshot.  Recorded on blueprints as `signal_state_hash` and
    /// validated by the EIL before simulation (§6).
    pub state_hash: B256,

    /// Signal payload — strategy-specific.  Encoded as JSON for
    /// cross-crate flexibility; each strategy deserialises into its
    /// own domain types.
    ///
    /// Payload schemas per SignalKind:
    ///   SpotPrice    → `{ "token": "0x…", "price_usd_e18": "…" }`
    ///   PoolReserves → `{ "pool": "0x…", "reserve0": "…", "reserve1": "…" }`
    ///   HealthFactor → `{ "position": "0x…", "hf_e18": "…", "protocol": "aave_v3" }`
    ///   OrderFlow    → `{ "tx_hash": "0x…", "decoded_swap": { … } }`
    ///   FeeOracle    → `{ "base_fee_gwei": N, "l1_data_fee_gwei": N, "priority_fee_gwei": N }`
    pub payload: serde_json::Value,
}
